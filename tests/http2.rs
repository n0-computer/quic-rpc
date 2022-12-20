#![cfg(feature = "http2")]
use std::{
    error, io,
    net::SocketAddr,
    pin::Pin,
    result,
    task::{Context, Poll},
};

use futures::{Future, FutureExt, SinkExt, Stream, StreamExt};
use hyper::{
    client::connect::{Connected, Connection},
    Uri,
};
use quic_rpc::{transport::http2, RpcClient, RpcServer};
use tokio::{io::AsyncRead, io::AsyncWrite, task::JoinHandle};

mod math;
use math::*;
use tower::Service;
mod util;

fn run_server_local(
    stream: impl Stream<
            Item = result::Result<
                impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
                impl Into<Box<dyn error::Error + Send + Sync>> + 'static,
            >,
        > + Send
        + 'static,
) -> JoinHandle<anyhow::Result<()>> {
    let accept = hyper::server::accept::from_stream(stream);
    let channel = http2::ServerChannel::<ComputeRequest, ComputeResponse>::serve_with_incoming(
        accept,
        Default::default(),
    )
    .unwrap();
    let server = RpcServer::<ComputeService, http2::ChannelTypes>::new(channel);
    tokio::spawn(async move {
        loop {
            let server = server.clone();
            ComputeService::server(server).await?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    })
}

fn run_server(addr: &SocketAddr) -> JoinHandle<anyhow::Result<()>> {
    let channel = http2::ServerChannel::<ComputeRequest, ComputeResponse>::serve(addr).unwrap();
    let server = RpcServer::<ComputeService, http2::ChannelTypes>::new(channel);
    tokio::spawn(async move {
        loop {
            let server = server.clone();
            ComputeService::server(server).await?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    })
}

#[derive(Clone)]
struct MemConnection {
    send: Option<flume::r#async::SendSink<'static, Vec<u8>>>,
    recv: flume::r#async::RecvStream<'static, Vec<u8>>,
    recv_buf: Vec<u8>,
}

impl MemConnection {
    fn pair() -> (Self, Self) {
        let (send1, recv1) = flume::unbounded();
        let (send2, recv2) = flume::unbounded();
        (
            MemConnection {
                send: Some(send1.into_sink()),
                recv: recv2.into_stream(),
                recv_buf: Vec::new(),
            },
            MemConnection {
                send: Some(send2.into_sink()),
                recv: recv1.into_stream(),
                recv_buf: Vec::new(),
            },
        )
    }
}

impl AsyncRead for MemConnection {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        while let Poll::Ready(Some(buffer)) = self.recv.poll_next_unpin(cx) {
            self.recv_buf.extend_from_slice(&buffer);
        }
        if self.recv_buf.is_empty() {
            return Poll::Pending;
        }
        let n = std::cmp::min(buf.remaining(), self.recv_buf.len());
        buf.put_slice(&self.recv_buf[..n]);
        self.recv_buf.drain(..n);
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for MemConnection {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        match &mut self.send {
            Some(send) => match send.poll_ready_unpin(cx) {
                Poll::Ready(_) => Poll::Ready(
                    send.start_send_unpin(buf.to_vec())
                        .map(|_| buf.len())
                        .map_err(|e| io::Error::new(io::ErrorKind::Other, e)),
                ),
                Poll::Pending => Poll::Pending,
            },
            None => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "closed",
            ))),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        self.send.take();
        Poll::Ready(Ok(()))
    }
}

impl Connection for MemConnection {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[derive(Debug, Clone)]
struct TestService(flume::Sender<MemConnection>);

impl TestService {
    fn new() -> (Self, flume::Receiver<MemConnection>) {
        let (sender, receiver) = flume::unbounded();
        (Self(sender), receiver)
    }
}

impl Service<hyper::Uri> for TestService {
    type Response = MemConnection;
    type Error = flume::SendError<MemConnection>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: hyper::Uri) -> Self::Future {
        println!("calling {}", _req);
        let (local, remote) = MemConnection::pair();
        let sender = self.0.clone();
        async move {
            sender.send_async(remote).await?;
            println!("returning channel");
            Ok(local)
        }
        .boxed()
    }
}

#[tokio::test]
async fn http2_channel_bench_local() -> anyhow::Result<()> {
    type C = http2::ChannelTypes;
    // dummy addr
    let uri = "http://[..]:50051".parse()?;
    let (service, server) = TestService::new();
    let server = server.into_stream().map(anyhow::Ok);
    let server_handle = run_server_local(server);
    let client = http2::ClientChannel::new_with_connector(service, uri, Default::default());
    // let client = RpcClient::<ComputeService, C>::new(client);
    smoke_test::<C>(client).await?;
    println!("terminating server");
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
async fn http2_channel_bench() -> anyhow::Result<()> {
    type C = http2::ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = http2::ClientChannel::new(uri);
    let client = RpcClient::<ComputeService, C>::new(client);
    bench(client, 50000).await?;
    println!("terminating server");
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
#[ignore]
async fn http2_channel_smoke() -> anyhow::Result<()> {
    type C = http2::ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = http2::ClientChannel::new(uri);
    smoke_test::<C>(client).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}
