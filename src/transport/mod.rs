//! Transports for quic-rpc
use futures_lite::{Future, Stream};
use futures_sink::Sink;
use futures_util::{SinkExt, StreamExt};
use pin_project::pin_project;

use crate::{RpcError, RpcMessage};
use std::{
    fmt::{self, Debug, Display},
    io,
    net::SocketAddr,
    pin::Pin,
};
#[cfg(feature = "combined-transport")]
pub mod combined;
#[cfg(feature = "flume-transport")]
pub mod flume;
#[cfg(feature = "hyper-transport")]
pub mod hyper;
#[cfg(feature = "interprocess-transport")]
pub mod interprocess;
#[cfg(feature = "quinn-transport")]
pub mod quinn;
#[cfg(feature = "quinn-flume-socket")]
pub mod quinn_flume_socket;

pub mod misc;

enum SendSinkInner<T: RpcMessage> {
    Direct(::flume::r#async::SendSink<'static, T>),
    Boxed(Pin<Box<dyn Sink<T, Error = std::io::Error> + Send + Sync + 'static>>),
}

///
#[derive(Debug)]
#[pin_project]
pub enum SendError {
    /// The receiver was dropped for a memory channel or a memory forwarder of a network channel
    ReceiverDropped,
    /// We got an IO error from the networking layer
    Io(io::Error),
}

impl Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::ReceiverDropped => write!(f, "ReceiverDropped"),
            SendError::Io(e) => write!(f, "Io({})", e),
        }
    }
}

/// A sink that can be used to send messages to the remote end of a channel.
///
/// For local channels, this is a thin wrapper around a flume send sink.
/// For network channels, this contains a boxed sink, since it is reasonable
/// to assume that in that case the additional overhead of boxing is negligible.
#[pin_project]
pub struct SendSink<T: RpcMessage>(SendSinkInner<T>);

impl<T: RpcMessage> SendSink<T> {
    /// Create a new send sink from a boxed sink
    pub fn boxed(sink: Pin<Box<dyn Sink<T, Error = io::Error> + Send + Sync + 'static>>) -> Self {
        Self(SendSinkInner::Boxed(sink))
    }

    /// Create a new send sink from a direct flume send sink
    pub(crate) fn direct(sink: ::flume::r#async::SendSink<'static, T>) -> Self {
        Self(SendSinkInner::Direct(sink))
    }
}

impl<T: RpcMessage> Sink<T> for SendSink<T> {
    type Error = SendError;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match self.project().0 {
            SendSinkInner::Direct(sink) => sink
                .poll_ready_unpin(cx)
                .map_err(|_| SendError::ReceiverDropped),
            SendSinkInner::Boxed(sink) => sink.poll_ready_unpin(cx).map_err(SendError::Io),
        }
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        match self.project().0 {
            SendSinkInner::Direct(sink) => sink
                .start_send_unpin(item)
                .map_err(|_| SendError::ReceiverDropped),
            SendSinkInner::Boxed(sink) => sink.start_send_unpin(item).map_err(SendError::Io),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match self.project().0 {
            SendSinkInner::Direct(sink) => sink
                .poll_flush_unpin(cx)
                .map_err(|_| SendError::ReceiverDropped),
            SendSinkInner::Boxed(sink) => sink.poll_flush_unpin(cx).map_err(SendError::Io),
        }
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        match self.project().0 {
            SendSinkInner::Direct(sink) => sink
                .poll_close_unpin(cx)
                .map_err(|_| SendError::ReceiverDropped),
            SendSinkInner::Boxed(sink) => sink.poll_close_unpin(cx).map_err(SendError::Io),
        }
    }
}

enum RecvStreamInner<T: RpcMessage> {
    Direct(::flume::r#async::RecvStream<'static, T>),
    Boxed(Pin<Box<dyn Stream<Item = Result<T, io::Error>> + Send + Sync + 'static>>),
}

/// A stream that can be used to receive messages from the remote end of a channel.
///
/// For local channels, this is a thin wrapper around a flume receive stream.
/// For network channels, this contains a boxed stream, since it is reasonable
#[pin_project]
pub struct RecvStream<T: RpcMessage>(RecvStreamInner<T>);

impl<T: RpcMessage> RecvStream<T> {
    /// Create a new receive stream from a boxed stream
    pub fn boxed(
        stream: Pin<Box<dyn Stream<Item = Result<T, io::Error>> + Send + Sync + 'static>>,
    ) -> Self {
        Self(RecvStreamInner::Boxed(stream))
    }

    /// Create a new receive stream from a direct flume receive stream
    pub(crate) fn direct(stream: ::flume::r#async::RecvStream<'static, T>) -> Self {
        Self(RecvStreamInner::Direct(stream))
    }
}

impl<T: RpcMessage> Stream for RecvStream<T> {
    type Item = Result<T, io::Error>;

    fn poll_next(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.project().0 {
            RecvStreamInner::Direct(stream) => match stream.poll_next_unpin(cx) {
                std::task::Poll::Ready(Some(item)) => std::task::Poll::Ready(Some(Ok(item))),
                std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
                std::task::Poll::Pending => std::task::Poll::Pending,
            },
            RecvStreamInner::Boxed(stream) => stream.poll_next_unpin(cx),
        }
    }
}

#[cfg(any(feature = "quinn-transport", feature = "hyper-transport"))]
mod util;

/// Errors that can happen when creating and using a [`Connection`] or [`ServerEndpoint`].
pub trait ConnectionErrors: Debug + Clone + Send + Sync + 'static {
    /// Error when opening or accepting a channel
    type OpenError: RpcError;
    /// Error when sending a message via a channel
    type SendError: RpcError;
    /// Error when receiving a message via a channel
    type RecvError: RpcError;
}

/// Types that are common to both [`Connection`] and [`ServerEndpoint`].
///
/// Having this as a separate trait is useful when writing generic code that works with both.
pub trait ConnectionCommon<In, Out>: ConnectionErrors {
    /// Receive side of a bidirectional typed channel
    type RecvStream: Stream<Item = Result<In, Self::RecvError>> + Send + Sync + Unpin + 'static;
    /// Send side of a bidirectional typed channel
    type SendSink: Sink<Out, Error = Self::SendError> + Send + Sync + Unpin + 'static;
}

/// A connection to a specific remote machine
///
/// A connection can be used to open bidirectional typed channels using [`Connection::open_bi`].
pub trait Connection<In, Out>: ConnectionCommon<In, Out> {
    /// Open a channel to the remote side
    fn open_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>> + Send;
}

/// A server endpoint that listens for connections
///
/// A server endpoint can be used to accept bidirectional typed channels from any of the
/// currently opened connections to clients, using [`ServerEndpoint::accept_bi`].
pub trait ServerEndpoint<In, Out>: ConnectionCommon<In, Out> {
    /// Accept a new typed bidirectional channel on any of the connections we
    /// have currently opened.
    fn accept_bi(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>> + Send;

    /// The local addresses this endpoint is bound to.
    fn local_addr(&self) -> &[LocalAddr];
}

/// The kinds of local addresses a [ServerEndpoint] can be bound to.
///
/// Returned by [ServerEndpoint::local_addr].
///
/// [`Display`]: fmt::Display
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum LocalAddr {
    /// A local socket.
    Socket(SocketAddr),
    /// An in-memory address.
    Mem,
}

impl Display for LocalAddr {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            LocalAddr::Socket(sockaddr) => write!(f, "{sockaddr}"),
            LocalAddr::Mem => write!(f, "mem"),
        }
    }
}
