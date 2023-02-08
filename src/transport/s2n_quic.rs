//! QUIC channel implementation based on quinn
use crate::{LocalAddr, RpcMessage};
use futures::channel::oneshot;
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use s2n_quic::stream::BidirectionalStream;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Poll, ready};
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

#[derive(Debug)]
struct ServerChannelInner {
    task: tokio::task::JoinHandle<()>,
    recv: flume::Receiver<Result<s2n_quic::stream::BidirectionalStream, s2n_quic::connection::Error>>,
    local_addr: [LocalAddr; 1],
}

/// A server channel using a quinn connection
#[derive(Debug)]
pub struct ServerChannel<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ServerChannelInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> ServerChannel<In, Out> {
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
            inner: Arc::new(ServerChannelInner {
                task,
                recv,
                local_addr: [LocalAddr::Socket(local_addr)],
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

#[derive(Debug)]
struct ClientChannelInner {
    task: tokio::task::JoinHandle<()>,
    send: flume::Sender<futures::channel::oneshot::Sender<BidirectionalStream>>,
}

/// A server channel using a quinn connection
pub struct ClientChannel<In: RpcMessage, Out: RpcMessage>(
    Arc<ClientChannelInner>,
    PhantomData<(In, Out)>,
);

impl<In: RpcMessage, Out: RpcMessage> ClientChannel<In, Out> {
    /// Create a new channel
    pub fn new(client: s2n_quic::Client, connect: s2n_quic::client::Connect) -> Self {
        let (send, recv) = flume::bounded::<futures::channel::oneshot::Sender<BidirectionalStream>>(1);
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
            Arc::new(ClientChannelInner {
                send,
                task,
            }),
            PhantomData,
        )
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientChannel")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ClientChannel<In, Out> {
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

/// Types for quinn channels.
///
/// This exposes the types from quinn directly without attempting to wrap them.
#[derive(Debug, Clone, Copy)]
pub struct ChannelTypes;

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<'a, In, Out> {
    inner: futures::channel::oneshot::Receiver<BidirectionalStream>,
    p: PhantomData<&'a (In, Out)>,
}

impl<'a, In, Out> Future for OpenBiFuture<'a, In, Out> {
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

enum ServerConnectionState {
    Initial,
    Connected,
    Final,
}

/// Future returned by accept_bi
#[pin_project]
pub struct AcceptBiFuture<'a, In, Out> {
    inner: flume::r#async::RecvFut<'a, Result<BidirectionalStream, s2n_quic::connection::Error>>,
    p: PhantomData<(In, Out)>,
}

impl<'a, In, Out> Future for AcceptBiFuture<'a, In, Out> {
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

impl crate::ChannelTypes for ChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type OpenBiError = self::OpenBiError;

    type AcceptBiError = self::OpenBiError;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::ClientChannel<In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::ServerChannel<In, Out>;
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ClientChannel<In, Out, ChannelTypes>
    for self::ClientChannel<In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        let (tx, rx) = oneshot::channel();
        self.0.send.send(tx).unwrap();
        OpenBiFuture {
            p: PhantomData,
            inner: rx,
        }
    }
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ServerChannel<In, Out, ChannelTypes>
    for self::ServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture {
            inner: self.inner.recv.recv_async(),
            p: PhantomData,
        }
    }

    fn local_addr(&self) -> &[crate::LocalAddr] {
        todo!()
    }
}

/// CreateChannelError for quinn channels.
#[derive(Debug, Clone)]
pub enum CreateChannelError {
    /// Something went wrong immediately when creating the quinn endpoint
    Io(io::ErrorKind, String),
    /// Error directly when calling connect on the quinn endpoint
    Connect(quinn::ConnectError),
    /// Error produced by the future returned by connect
    Connection(quinn::ConnectionError),
}

impl From<io::Error> for CreateChannelError {
    fn from(e: io::Error) -> Self {
        CreateChannelError::Io(e.kind(), e.to_string())
    }
}

impl From<quinn::ConnectionError> for CreateChannelError {
    fn from(e: quinn::ConnectionError) -> Self {
        CreateChannelError::Connection(e)
    }
}

impl From<quinn::ConnectError> for CreateChannelError {
    fn from(e: quinn::ConnectError) -> Self {
        CreateChannelError::Connect(e)
    }
}

impl fmt::Display for CreateChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CreateChannelError {}
