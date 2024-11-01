//! Transports for quic-rpc
//!
//! There are two sides to a transport, a server side where connections are
//! accepted and a client side where connections are initiated.
//!
//! Connections are bidirectional typed channels, with a distinct type for
//! the send and receive side. They are *unrelated* to services.
//!
//! In the transport module, the message types are referred to as `In` and `Out`.
//!
//! A [`Connection`] can be used to *open* bidirectional typed channels using
//! [`Connection::open`]. A [`ServerEndpoint`] can be used to *accept* bidirectional
//! typed channels from any of the currently opened connections to clients, using
//! [`ServerEndpoint::accept`].
//!
//! In both cases, the result is a tuple of a send side and a receive side. These
//! types are defined by implementing the [`ConnectionCommon`] trait.
//!
//! Errors for both sides are defined by implementing the [`ConnectionErrors`] trait.
use futures_lite::{Future, Stream};
use futures_sink::Sink;

use crate::{RpcError, RpcMessage};
use std::{
    fmt::{self, Debug, Display},
    net::SocketAddr,
};
#[cfg(feature = "flume-transport")]
pub mod boxed;
#[cfg(feature = "combined-transport")]
pub mod combined;
#[cfg(feature = "flume-transport")]
pub mod flume;
#[cfg(feature = "hyper-transport")]
pub mod hyper;
#[cfg(feature = "quinn-transport")]
pub mod quinn;

pub mod misc;

#[cfg(any(feature = "quinn-transport", feature = "hyper-transport"))]
mod util;

pub mod mapped;

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
pub trait ConnectionCommon: ConnectionErrors {
    /// The type of messages that can be received on the channel
    type In: RpcMessage;
    /// The type of messages that can be sent on the channel
    type Out: RpcMessage;
    /// Receive side of a bidirectional typed channel
    type RecvStream: Stream<Item = Result<Self::In, Self::RecvError>>
        + Send
        + Sync
        + Unpin
        + 'static;
    /// Send side of a bidirectional typed channel
    type SendSink: Sink<Self::Out, Error = Self::SendError> + Send + Sync + Unpin + 'static;
}

/// A connection to a specific remote machine
///
/// A connection can be used to open bidirectional typed channels using [`Connection::open`].
pub trait Connection: ConnectionCommon {
    /// Open a channel to the remote che
    fn open(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>> + Send;
}

/// A server endpoint that listens for connections
///
/// A server endpoint can be used to accept bidirectional typed channels from any of the
/// currently opened connections to clients, using [`ServerEndpoint::accept`].
pub trait ServerEndpoint: ConnectionCommon {
    /// Accept a new typed bidirectional channel on any of the connections we
    /// have currently opened.
    fn accept(
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
