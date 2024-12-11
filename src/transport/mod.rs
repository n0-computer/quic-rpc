//! Built in transports for quic-rpc
//!
//! There are two sides to a transport, a server side where connections are
//! accepted and a client side where connections are initiated.
//!
//! Connections are bidirectional typed channels, with a distinct type for
//! the send and receive side. They are *unrelated* to services.
//!
//! In the transport module, the message types are referred to as `In` and `Out`.
//!
//! A [`Connector`] can be used to *open* bidirectional typed channels using
//! [`Connector::open`]. A [`Listener`] can be used to *accept* bidirectional
//! typed channels from any of the currently opened connections to clients, using
//! [`Listener::accept`].
//!
//! In both cases, the result is a tuple of a send side and a receive side. These
//! types are defined by implementing the [`StreamTypes`] trait.
//!
//! Errors for both sides are defined by implementing the [`ConnectionErrors`] trait.
use std::{
    fmt::{self, Debug, Display},
    net::SocketAddr,
};

use boxed::{BoxableConnector, BoxableListener, BoxedConnector, BoxedListener};
use futures_lite::{Future, Stream};
use futures_sink::Sink;
use mapped::MappedConnector;

use crate::{RpcError, RpcMessage};

pub mod boxed;
pub mod combined;
#[cfg(feature = "flume-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
pub mod flume;
#[cfg(feature = "hyper-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "hyper-transport")))]
pub mod hyper;
#[cfg(feature = "iroh-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "iroh-transport")))]
pub mod iroh;
pub mod mapped;
pub mod misc;
#[cfg(feature = "quinn-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "quinn-transport")))]
pub mod quinn;

#[cfg(any(feature = "quinn-transport", feature = "iroh-transport"))]
#[cfg_attr(
    iroh_docsrs,
    doc(cfg(any(feature = "quinn-transport", feature = "iroh-transport")))
)]
mod util;

/// Errors that can happen when creating and using a [`Connector`] or [`Listener`].
pub trait ConnectionErrors: Debug + Clone + Send + Sync + 'static {
    /// Error when sending a message via a channel
    type SendError: RpcError;
    /// Error when receiving a message via a channel
    type RecvError: RpcError;
    /// Error when opening a channel
    type OpenError: RpcError;
    /// Error when accepting a channel
    type AcceptError: RpcError;
}

/// Types that are common to both [`Connector`] and [`Listener`].
///
/// Having this as a separate trait is useful when writing generic code that works with both.
pub trait StreamTypes: ConnectionErrors {
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
/// A connection can be used to open bidirectional typed channels using [`Connector::open`].
pub trait Connector: StreamTypes {
    /// Open a channel to the remote che
    fn open(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>> + Send;

    /// Map the input and output types of this connection
    fn map<In1, Out1>(self) -> MappedConnector<In1, Out1, Self>
    where
        In1: TryFrom<Self::In>,
        Self::Out: From<Out1>,
    {
        MappedConnector::new(self)
    }

    /// Box the connection
    fn boxed(self) -> BoxedConnector<Self::In, Self::Out>
    where
        Self: BoxableConnector<Self::In, Self::Out> + Sized + 'static,
    {
        self::BoxedConnector::new(self)
    }
}

/// A listener that listens for connections
///
/// A listener can be used to accept bidirectional typed channels from any of the
/// currently opened connections to clients, using [`Listener::accept`].
pub trait Listener: StreamTypes {
    /// Accept a new typed bidirectional channel on any of the connections we
    /// have currently opened.
    fn accept(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::AcceptError>> + Send;

    /// The local addresses this endpoint is bound to.
    fn local_addr(&self) -> &[LocalAddr];

    /// Box the listener
    fn boxed(self) -> BoxedListener<Self::In, Self::Out>
    where
        Self: BoxableListener<Self::In, Self::Out> + Sized + 'static,
    {
        BoxedListener::new(self)
    }
}

/// The kinds of local addresses a [Listener] can be bound to.
///
/// Returned by [Listener::local_addr].
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
