//! Transports for quic-rpc
use futures_lite::{Future, Stream};
use futures_sink::Sink;

use crate::RpcError;
use std::{
    fmt::{self, Debug, Display},
    net::SocketAddr,
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
    /// The future that will resolve to a substream or an error
    type OpenBiFut: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + Send;
    /// Open a channel to the remote
    fn open_bi(&self) -> Self::OpenBiFut;
}

/// A server endpoint that listens for connections
///
/// A server endpoint can be used to accept bidirectional typed channels from any of the
/// currently opened connections to clients, using [`ServerEndpoint::accept_bi`].
pub trait ServerEndpoint<In, Out>: ConnectionCommon<In, Out> {
    /// The future that will resolve to a substream or an error
    type AcceptBiFut: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + Send;

    /// Accept a new typed bidirectional channel on any of the connections we
    /// have currently opened.
    fn accept_bi(&self) -> Self::AcceptBiFut;

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
