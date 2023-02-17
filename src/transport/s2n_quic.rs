//! QUIC channel implementation based on quinn
use crate::RpcMessage;
use futures::channel::oneshot;
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use s2n_quic::stream::BidirectionalStream;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{ready, Poll};
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use super::{ConnectionCommon, ConnectionErrors, LocalAddr};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

#[derive(Debug)]
struct ServerEndpointInner {
    task: Option<tokio::task::JoinHandle<()>>,
    recv:
        flume::Receiver<Result<s2n_quic::stream::BidirectionalStream, s2n_quic::connection::Error>>,
    local_addr: [LocalAddr; 1],
}

impl Drop for ServerEndpointInner {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A server channel using a quinn connection
#[derive(Debug)]
pub struct S2nQuicServerEndpoint<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ServerEndpointInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> S2nQuicServerEndpoint<In, Out> {
    /// Create a new channel
    pub fn new(mut server: s2n_quic::Server, local_addr: SocketAddr) -> Self {
        let (send, recv) = flume::bounded(1);
        let task = tokio::spawn(async move {
            'outer: while let Some(mut connection) = server.accept().await {
                while let Some(res) = connection.accept_bidirectional_stream().await.transpose() {
                    if send.send_async(res).await.is_err() {
                        break 'outer;
                    }
                }
            }
        });
        Self {
            inner: Arc::new(ServerEndpointInner {
                task: Some(task),
                recv,
                local_addr: [LocalAddr::Socket(local_addr)],
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for S2nQuicServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

#[derive(Debug)]
struct ConnectionInner {
    task: Option<tokio::task::JoinHandle<()>>,
    send: flume::Sender<futures::channel::oneshot::Sender<BidirectionalStream>>,
}

impl Drop for ConnectionInner {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A server channel using a quinn connection
pub struct S2nQuicConnection<In: RpcMessage, Out: RpcMessage>(
    Arc<ConnectionInner>,
    PhantomData<(In, Out)>,
);

impl<In: RpcMessage, Out: RpcMessage> S2nQuicConnection<In, Out> {
    /// Create a new channel
    pub fn new(client: s2n_quic::Client, connect: s2n_quic::client::Connect) -> Self {
        let (send, recv) =
            flume::bounded::<futures::channel::oneshot::Sender<BidirectionalStream>>(1);
        let task = tokio::spawn(async move {
            loop {
                let connect = connect.clone();
                let mut connection = match client.connect(connect).await {
                    Ok(conn) => conn,
                    Err(_) => continue,
                };
                loop {
                    let sender = match recv.recv_async().await {
                        Ok(sender) => sender,
                        Err(_) => break,
                    };
                    let stream = match connection.open_bidirectional_stream().await {
                        Ok(stream) => stream,
                        Err(_) => continue,
                    };
                    let _ = sender.send(stream);
                }
            }
        });
        Self(
            Arc::new(ConnectionInner {
                send,
                task: Some(task),
            }),
            PhantomData,
        )
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for S2nQuicConnection<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientChannel")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for S2nQuicConnection<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct SendSink<Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedWrite<::s2n_quic::stream::SendStream, LengthDelimitedCodec>,
        Out,
        SymmetricalBincode<Out>,
    >,
);

impl<Out: Serialize> Sink<Out> for SendSink<Out> {
    type Error = io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        self.project().0.start_send_unpin(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

/// A stream that wraps a quinn RecvStream with length delimiting and bincode
#[pin_project]
pub struct RecvStream<In>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedRead<::s2n_quic::stream::ReceiveStream, LengthDelimitedCodec>,
        In,
        SymmetricalBincode<In>,
    >,
);

impl<In: DeserializeOwned> Stream for RecvStream<In> {
    type Item = result::Result<In, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

/// Error for open_bi. Currently just a quinn::ConnectionError
pub type OpenBiError = s2n_quic::connection::Error;

/// Error for accept_bi. Currently just a quinn::ConnectionError
pub type AcceptBiError = s2n_quic::connection::Error;

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<In, Out> {
    inner: futures::channel::oneshot::Receiver<BidirectionalStream>,
    p: PhantomData<(In, Out)>,
}

impl<In, Out> Future for OpenBiFuture<In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let stream = ready!(self.inner.poll_unpin(cx)).unwrap();
        Poll::Ready(Ok(wrap_bidi_stream(stream)))
    }
}

fn wrap_bidi_stream<In, Out>(stream: BidirectionalStream) -> (SendSink<Out>, RecvStream<In>) {
    let (recv, send) = stream.split();
    let send = FramedWrite::new(send, LengthDelimitedCodec::new());
    let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
    // now switch to streams of WantRequestUpdate and WantResponse
    let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
    let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
    // box so we don't have to write down the insanely long type
    let send = SendSink(send);
    let recv = RecvStream(recv);
    (send, recv)
}

/// Future returned by accept_bi
#[pin_project]
pub struct AcceptBiFuture<In, Out> {
    inner:
        flume::r#async::RecvFut<'static, Result<BidirectionalStream, s2n_quic::connection::Error>>,
    p: PhantomData<(In, Out)>,
}

impl<In, Out> Future for AcceptBiFuture<In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::AcceptBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let res = ready!(self.project().inner.poll_unpin(cx)).unwrap()?;
        Poll::Ready(Ok(wrap_bidi_stream(res)))
    }
}

// pub type AcceptBiFuture<'a, In, Out> =
//     BoxFuture<'a, result::Result<self::Socket<In, Out>, self::AcceptBiError>>;

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for self::S2nQuicConnection<In, Out> {
    type OpenError = s2n_quic::connection::Error;
    type SendError = io::Error;
    type RecvError = io::Error;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out>
    for self::S2nQuicConnection<In, Out>
{
    type RecvStream = self::RecvStream<In>;
    type SendSink = self::SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> super::Connection<In, Out>
    for self::S2nQuicConnection<In, Out>
{
    fn open_bi(&self) -> Self::OpenBiFut {
        let (tx, rx) = oneshot::channel();
        self.0.send.send(tx).unwrap();
        OpenBiFuture {
            p: PhantomData,
            inner: rx,
        }
    }

    type OpenBiFut = self::OpenBiFuture<In, Out>;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for self::S2nQuicServerEndpoint<In, Out> {
    type OpenError = s2n_quic::connection::Error;
    type SendError = io::Error;
    type RecvError = io::Error;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out>
    for self::S2nQuicServerEndpoint<In, Out>
{
    type RecvStream = self::RecvStream<In>;
    type SendSink = self::SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> super::ServerEndpoint<In, Out>
    for self::S2nQuicServerEndpoint<In, Out>
{
    fn accept_bi(&self) -> Self::AcceptBiFut {
        AcceptBiFuture {
            inner: self.inner.recv.clone().into_recv_async(),
            p: PhantomData,
        }
    }

    fn local_addr(&self) -> &[LocalAddr] {
        self.inner.local_addr.as_slice()
    }

    type AcceptBiFut = self::AcceptBiFuture<In, Out>;
}
