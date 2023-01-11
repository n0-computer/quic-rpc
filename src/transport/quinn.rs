//! QUIC channel implementation based on quinn
use crate::{LocalAddr, RpcMessage};
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

/// A server channel using a quinn connection
#[derive(Debug)]
pub struct QuinnServerChannel<In: RpcMessage, Out: RpcMessage> {
    connection: quinn::Connection,
    local_addr: [LocalAddr; 1],
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnServerChannel<In, Out> {
    /// Create a new channel
    pub fn new(conn: quinn::Connection, local_addr: SocketAddr) -> Self {
        Self {
            connection: conn,
            local_addr: [LocalAddr::Socket(local_addr)],
            _phantom: PhantomData,
        }
    }
}

// impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ServerChannel<In, Out> {
//     fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
//         f.debug_tuple("ServerChannel")
//             .field(&self.0)
//             .field(&self.1)
//             .finish()
//     }
// }

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            connection: self.connection.clone(),
            local_addr: self.local_addr.clone(),
            _phantom: PhantomData,
        }
    }
}

/// A server channel using a quinn connection
pub struct QuinnClientChannel<In: RpcMessage, Out: RpcMessage>(
    quinn::Connection,
    PhantomData<(In, Out)>,
);

impl<In: RpcMessage, Out: RpcMessage> QuinnClientChannel<In, Out> {
    /// Create a new channel
    pub fn new(conn: quinn::Connection) -> Self {
        Self(conn, PhantomData)
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for QuinnClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientChannel")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct SendSink<Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedWrite<::quinn::SendStream, LengthDelimitedCodec>,
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
        FramedRead<::quinn::RecvStream, LengthDelimitedCodec>,
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
pub type OpenBiError = quinn::ConnectionError;

/// Error for accept_bi. Currently just a quinn::ConnectionError
pub type AcceptBiError = quinn::ConnectionError;

/// Types for quinn channels.
///
/// This exposes the types from quinn directly without attempting to wrap them.
#[derive(Debug, Clone, Copy)]
pub struct QuinnChannelTypes;

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<'a, In, Out>(#[pin] quinn::OpenBi<'a>, PhantomData<(In, Out)>);

impl<'a, In, Out> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.project().0.poll_unpin(cx).map(|conn| {
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            // box so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        })
    }
}

/// Future returned by accept_bi
#[pin_project]
pub struct AcceptBiFuture<'a, In, Out>(#[pin] quinn::AcceptBi<'a>, PhantomData<(In, Out)>);

impl<'a, In, Out> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.project().0.poll_unpin(cx).map(|conn| {
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            // box so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        })
    }
}

// pub type AcceptBiFuture<'a, In, Out> =
//     BoxFuture<'a, result::Result<self::Socket<In, Out>, self::AcceptBiError>>;

impl crate::ChannelTypes for QuinnChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type OpenBiError = self::OpenBiError;

    type AcceptBiError = self::OpenBiError;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::QuinnClientChannel<In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::QuinnServerChannel<In, Out>;
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ClientChannel<In, Out, QuinnChannelTypes>
    for self::QuinnClientChannel<In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        OpenBiFuture(self.0.open_bi(), PhantomData)
    }
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ServerChannel<In, Out, QuinnChannelTypes>
    for self::QuinnServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(self.connection.accept_bi(), PhantomData)
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
