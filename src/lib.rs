//! A streaming rpc system based on quic
//!
//! # Motivation
//!
//! See the [README](https://github.com/n0-computer/quic-rpc/blob/main/README.md)
//!
//! # Example
//! ```
//! # async fn example() -> anyhow::Result<()> {
//! use quic_rpc::{message::RpcMsg, Service, RpcClient, transport::MemChannelTypes};
//! use serde::{Serialize, Deserialize};
//! use derive_more::{From, TryInto};
//!
//! // Define your messages
//! #[derive(Debug, Serialize, Deserialize)]
//! struct Ping;
//!
//! #[derive(Debug, Serialize, Deserialize)]
//! struct Pong;
//!
//! // Define your RPC service and its request/response types
//!
//! #[derive(Debug, Serialize, Deserialize, From, TryInto)]
//! enum PingRequest {
//!     Ping(Ping),
//! }
//!
//! #[derive(Debug, Serialize, Deserialize, From, TryInto)]
//! enum PingResponse {
//!     Pong(Pong),
//! }
//!
//! #[derive(Debug, Clone)]
//! struct PingService;
//!
//! impl Service for PingService {
//!   type Req = PingRequest;
//!   type Res = PingResponse;
//! }
//!
//! // Define interaction patterns for each request type
//! impl RpcMsg<PingService> for Ping {
//!   type Response = Pong;
//! }
//!
//! // create a transport channel
//! let (server, client) = quic_rpc::transport::mem::connection::<PingRequest, PingResponse>(1);
//!
//! // create the rpc client given the channel and the service type
//! let mut client = RpcClient::<PingService, _>::new(client);
//!
//! // call the service
//! let res = client.rpc(Ping).await?;
//! # Ok(())
//! # }
//! ```
// #![deny(missing_docs)]
// #![deny(rustdoc::broken_intra_doc_links)]
use futures::{Future, Sink, Stream};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fmt::{self, Debug, Display},
    net::SocketAddr,
};
pub mod client;
pub mod message;
pub mod server;
pub mod transport;
pub use client::RpcClient;
pub use server::RpcServer;
mod macros;

/// Requirements for a RPC message
///
/// Even when just using the mem transport, we require messages to be Serializable and Deserializable.
/// Likewise, even when using the quinn transport, we require messages to be Send.
///
/// This does not seem like a big restriction. If you want a pure memory channel without the possibility
/// to also use the quinn transport, you might want to use a mpsc channel directly.
pub trait RpcMessage: Debug + Serialize + DeserializeOwned + Send + Sync + Unpin + 'static {}

impl<T> RpcMessage for T where
    T: Debug + Serialize + DeserializeOwned + Send + Sync + Unpin + 'static
{
}

/// Requirements for an internal error
///
/// All errors have to be Send and 'static so they can be sent across threads.
pub trait RpcError: Debug + Display + Send + Sync + Unpin + 'static {}

impl<T> RpcError for T where T: Debug + Display + Send + Sync + Unpin + 'static {}

/// A service
pub trait Service: Send + Sync + Debug + Clone + 'static {
    /// Type of request messages
    type Req: RpcMessage;
    /// Type of response messages
    type Res: RpcMessage;
}

/// The kinds of local addresses a [`ServerChannel`] can be bound to.
///
/// Returned by [`ServerChannel::local_addr`].
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

/// Errors that can happen when creating and using a channel
///
/// This is independent of whether the channel is a byte channel or a message channel.
pub trait ConnectionErrors: Debug + Clone + Send + Sync + 'static {
    /// Error when opening a substream
    type OpenError: RpcError;
    /// Error when sending messages
    type SendError: RpcError;
    /// Error when receiving messages
    type RecvError: RpcError;
}

/// A connection to a specific remote machine
///
/// A connection is a source of bidirectional typed channels.
pub trait Connection<In, Out>: ConnectionErrors {
    /// A typed bidirectional message channel
    type RecvStream: Stream<Item = Result<In, Self::RecvError>> + Send + Unpin + 'static;
    type SendSink: Sink<Out, Error = Self::SendError> + Send + Unpin + 'static;
    /// The future that will resolve to a substream or an error
    type OpenBiFut<'a>: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + Send
        + 'a
    where
        Self: 'a;
    /// Open a channel to the remote
    fn open_bi(&self) -> Self::OpenBiFut<'_>;
}

/// A server endpoint that listens for connections
pub trait ServerEndpoint<In, Out>: ConnectionErrors {
    type RecvStream: Stream<Item = Result<In, Self::RecvError>> + Send + Unpin + 'static;
    type SendSink: Sink<Out, Error = Self::SendError> + Send + Unpin + 'static;
    /// The future that will resolve to a substream or an error
    type AcceptBiFut<'a>: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + Send
        + 'a
    where
        Self: 'a;

    /// Accept a new typed bidirectional channel on any of the connections we
    /// have currently opened.
    fn accept_bi(&self) -> Self::AcceptBiFut<'_>;
}

/// A connection to a specific service on a specific remote machine
/// 
/// This is just a trait alias for a [Connection] with the right types.
///
/// This can be used to create a [RpcClient] that can be used to send requests.
pub trait ServiceConnection<S: Service>: Connection<S::Res, S::Req> {}

impl<T: Connection<S::Res, S::Req>, S: Service> ServiceConnection<S> for T {}

/// A server endpoint for a specific service
///
/// This is just a trait alias for a [ServerEndpoint] with the right types.
///
/// This can be used to create a [RpcServer] that can be used to handle
/// requests.
pub trait ServiceEndpoint<S: Service>: ServerEndpoint<S::Req, S::Res> {}

impl<T: ServerEndpoint<S::Req, S::Res>, S: Service> ServiceEndpoint<S> for T {}
