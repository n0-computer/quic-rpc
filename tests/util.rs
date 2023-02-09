use std::{io, task::Poll, net::SocketAddr, pin::Pin, result};

use anyhow::Context;
use bytes::{Bytes, BytesMut};
use futures::{Future, StreamExt, SinkExt, future::BoxFuture, FutureExt};
use quic_rpc::{server::RpcServerError, ChannelTypes};
use tokio::io::{AsyncRead, AsyncWrite};

#[allow(unused)]
pub async fn check_termination_anyhow<C: ChannelTypes>(
    server_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(e) => {
            let err: RpcServerError<C> = e.downcast().context("unexpected termination result")?;
            match err {
                RpcServerError::AcceptBiError(_) => {}
                e => panic!("unexpected termination error {:?}", e),
            }
        }
        e => panic!("server should have terminated with an error {:?}", e),
    }
    Ok(())
}

trait BytesClient {
    type DataStream: SplittableStream;
    type OpenBiFuture<'a>: Future<Output = Self::DataStream> + 'a where Self: 'a;
    fn open_bi(&self) -> Self::OpenBiFuture<'_>;
}

struct QuinnClient {
    task: tokio::task::JoinHandle<result::Result<(), flume::SendError<Option<(quinn::SendStream, quinn::RecvStream)>>>>,
    receiver: flume::Receiver<Option<(quinn::SendStream, quinn::RecvStream)>>,
}

impl QuinnClient {
    fn new(addr: SocketAddr, name: String) -> io::Result<Self> {
        let endpoint = quinn::Endpoint::client(addr)?;
        let (sender, receiver) = flume::bounded::<Option<(quinn::SendStream, quinn::RecvStream)>>(0);
        let task = tokio::spawn(async move {
            // wait until there is a need to open a connection
            sender.send_async(None).await?;
            'outer: loop {
                let connecting = match endpoint.connect(addr, name.as_str()) {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!("failed to connect: {}", e);
                        continue;
                    }
                };
                let conn = match connecting.await {
                    Ok(conn) => conn,
                    Err(e) => {
                        tracing::warn!("failed to connect: {}", e);
                        continue;
                    }
                };
                // as long as the connection is usable, handle requests
                loop {
                    // open a new stream for each request
                    let pair = match conn.open_bi().await {
                        Ok(pair) => pair,
                        Err(e) => {
                            tracing::warn!("failed to create substream: {}", e);
                            tracing::warn!("reconnecting.");
                            continue 'outer;
                        }
                    };
                    sender.send_async(Some(pair)).await?;
                    // wait for more demand
                    sender.send_async(None).await?;
                }
            }
            Ok(())
        });
        Ok(Self { task, receiver })
    }
}

impl BytesClient for QuinnClient {
    type DataStream = (quinn::RecvStream, quinn::SendStream);
    type OpenBiFuture<'a> = BoxFuture<'a, Self::DataStream>;
    fn open_bi(&self) -> Self::OpenBiFuture<'_> {
        async move {
            loop {
                let x = self.receiver.recv_async().await.unwrap();
                if let Some((send, recv)) = x {
                    return (recv, send);
                }
            }
        }.boxed()
    }
}

trait SplittableStream {
    type RecvStream: AsyncRead;
    type SendStream: AsyncWrite;
    fn split(self) -> (Self::RecvStream, Self::SendStream);
}

// impl SplittableStream for s2n_quic::stream::BidirectionalStream {
//     type RecvStream = s2n_quic::stream::ReceiveStream;
//     type SendStream = s2n_quic::stream::SendStream;
//     fn split(self) -> (Self::RecvStream, Self::SendStream) {
//         self.split()
//     }
// }

impl SplittableStream for (quinn::RecvStream, quinn::SendStream) {
    type RecvStream = quinn::RecvStream;
    type SendStream = quinn::SendStream;
    fn split(self) -> (Self::RecvStream, Self::SendStream) {
        self
    }
}

impl SplittableStream for tokio::io::DuplexStream {
    type RecvStream = tokio::io::ReadHalf<tokio::io::DuplexStream>;

    type SendStream = tokio::io::WriteHalf<tokio::io::DuplexStream>;

    fn split(self) -> (Self::RecvStream, Self::SendStream) {
        tokio::io::split(self)
    }
}

impl SplittableStream for (flume::Sender<Bytes>, flume::Receiver<io::Result<Bytes>>) {
    type RecvStream = StreamReader;
    type SendStream = StreamSender;
    fn split(self) -> (Self::RecvStream, Self::SendStream) {
        let (tx, rx) = self;
        let rx = rx.into_stream();
        let sink = tx.into_sink();
        (StreamReader(ReaderState::Buffering(rx)), StreamSender { sink, buffer: BytesMut::with_capacity(1<<14) })
    }
}

struct StreamSender {
    sink: flume::r#async::SendSink<'static, Bytes>,
    buffer: BytesMut,
}

impl AsyncWrite for StreamSender {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        match self.sink.poll_ready_unpin(cx) {
            Poll::Ready(Ok(())) => {
                debug_assert!(self.buffer.is_empty());
                let n = buf.len().min(self.buffer.capacity());
                self.buffer.extend_from_slice(&buf[..n]);
                let item = self.buffer.split().freeze();
                self.sink
                    .start_send_unpin(item)
                    .unwrap();
                Poll::Ready(Ok(n))
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(io::ErrorKind::Other.into())),
            Poll::Pending => Poll::Pending,
        }
    }

    fn poll_flush(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: std::pin::Pin<&mut Self>, _cx: &mut std::task::Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

enum ReaderState {
    Buffering(flume::r#async::RecvStream<'static, io::Result<Bytes>>),
    Reading(flume::r#async::RecvStream<'static, io::Result<Bytes>>, Bytes),
    Taken,
}

struct StreamReader(ReaderState);

impl StreamReader {
    fn take(&mut self) -> ReaderState {
        std::mem::replace(&mut self.0, ReaderState::Taken)
    }
}

impl AsyncRead for StreamReader {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<io::Result<()>> {
        loop {
            match self.take() {
                ReaderState::Buffering(mut rx) => {
                    match rx.poll_next_unpin(cx) {
                        Poll::Ready(x) => {
                            match x {
                                Some(Ok(bytes)) => {
                                    // we got some bytes, become writing
                                    self.0 = ReaderState::Reading(rx, bytes);
                                }
                                Some(Err(e)) => {
                                    // we got an error, still reading
                                    self.0 = ReaderState::Buffering(rx);
                                    break Poll::Ready(Err(e))
                                }
                                None => {
                                    // EOF
                                    self.0 = ReaderState::Buffering(rx);
                                    break Poll::Ready(Ok(()))
                                }
                            }
                        }
                        Poll::Pending => {
                            self.0 = ReaderState::Buffering(rx);
                            break std::task::Poll::Pending
                        }
                    }
                    
                }
                ReaderState::Reading(rx, mut bytes) => {
                    let n = std::cmp::min(buf.remaining(), bytes.len());
                    buf.put_slice(&bytes[..n]);
                    bytes.split_to(n);
                    self.0 = if bytes.is_empty() {
                        ReaderState::Buffering(rx)
                    } else {
                        ReaderState::Reading(rx, bytes)
                    };
                    return std::task::Poll::Ready(Ok(()))
                }
                ReaderState::Taken => unreachable!(),
            }
        }
    }
}

#[tokio::test]
async fn flume_sink_test() {
    let (s, r) = flume::bounded::<Option<()>>(0);
    let task = tokio::spawn(async move {
        {
            // send item
            println!("checking demand");
            s.send_async(None).await;
            println!("doing the actual send");
            s.send_async(Some(())).await;
        }
    });
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    println!("creating demand");
    loop {
        let x = r.recv_async().await.unwrap();
        if x.is_some() {
            break;
        }
    }
    task.await.unwrap();
}
