#![cfg(feature = "http2")]
use std::{
    error, io,
    net::SocketAddr,
    pin::Pin,
    result,
    task::{Context, Poll},
};

use futures::{Future, FutureExt, Stream, StreamExt};
use hyper::{
    client::connect::{Connected, Connection},
    Uri,
};
use quic_rpc::{transport::http2, RpcClient, RpcServer};
use tokio::{io::AsyncRead, io::{AsyncWrite, DuplexStream, duplex}, task::JoinHandle};

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

#[pin_project::pin_project]
struct TestConnection(#[pin] DuplexStream);

impl AsyncRead for TestConnection {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.project().0.poll_read(cx, buf)
    }
}

impl AsyncWrite for TestConnection {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        self.project().0.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().0.poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        self.project().0.poll_shutdown(cx)
    }
}

impl Connection for TestConnection {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

#[derive(Debug, Clone)]
struct TestService(flume::Sender<TestConnection>);

impl TestService {
    fn new() -> (Self, flume::Receiver<TestConnection>) {
        let (sender, receiver) = flume::bounded(32);
        (Self(sender), receiver)
    }
}

impl Service<hyper::Uri> for TestService {
    type Response = TestConnection;
    type Error = flume::SendError<TestConnection>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, _req: hyper::Uri) -> Self::Future {
        println!("calling {}", _req);
        let (local, remote) = duplex(4096 * 1024);
        let sender = self.0.clone();
        async move {
            sender.send_async(TestConnection(remote)).await?;
            println!("returning channel");
            Ok(TestConnection(local))
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
    let client = RpcClient::<ComputeService, C>::new(client);
    bench(client, 50000).await?;
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
