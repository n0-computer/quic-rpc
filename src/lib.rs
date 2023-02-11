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

/// Groups types for client and server connections
pub trait ChannelTypes2 {
    /// The client connection type
    type ClientConnection<In: RpcMessage, Out: RpcMessage>: Connection<In, Out>;
    /// The server connection type
    type ServerConnection<In: RpcMessage, Out: RpcMessage>: Connection<In, Out>;
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

// #[derive(Debug)]
// struct ServerSource<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> {
//     channel: T::ServerChannel<In, Out>,
// }

// impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> Clone for ServerSource<In, Out, T> {
//     fn clone(&self) -> Self {
//         Self {
//             channel: self.channel.clone(),
//         }
//     }
// }

// impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> ConnectionErrors
//     for ServerSource<In, Out, T>
// {
//     type SendError = T::SendError;

//     type RecvError = T::RecvError;

//     type OpenError = T::AcceptBiError;
// }

// impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> TypedConnection<In, Out>
//     for ServerSource<In, Out, T>
// {
//     type RecvStream = T::RecvStream<In>;

//     type SendSink = T::SendSink<Out>;

//     type SubstreamFut<'a> = T::AcceptBiFuture<'a, In, Out>;

//     fn next(&self) -> Self::SubstreamFut<'_> {
//         self.channel.accept_bi()
//     }
// }

/// Errors that can happen when creating and using a channel
///
/// This is independent of whether the channel is a byte channel or a message channel.
pub trait ConnectionErrors: Debug + Clone + Send + Sync + 'static {
    /// Error when sending messages
    type SendError: RpcError;
    /// Error when receiving messages
    type RecvError: RpcError;
    /// Error when opening a substream
    type OpenError: RpcError;
}

/// A connection, aka a source of typed channels
///
/// Both the server and the client can be thought as a source of channels.
/// On the client, acquiring channels means open.
/// On the server, acquiring channels means accept.
pub trait Connection<In, Out>: ConnectionErrors {
    /// A typed bidirectional message channel
    type RecvStream: Stream<Item = Result<In, Self::RecvError>> + Send + Unpin + 'static;
    type SendSink: Sink<Out, Error = Self::SendError> + Send + Unpin + 'static;
    /// The future that will resolve to a substream or an error
    type NextFut<'a>: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + Send
        + 'a
    where
        Self: 'a;
    /// Get the next substream
    ///
    /// On the client side, this will open a new substream. This should complete
    /// immediately if the connection is already open.
    ///
    /// On the server side, this will accept a new substream. This can block
    /// indefinitely if no new client is interested.
    fn next(&self) -> Self::NextFut<'_>;
}

/// A client connection is a connection where requests are sent and responses are received
///
/// This is just a trait alias for TypedConnection<S::Res, S::Req>
pub trait ClientConnection<S: Service>: Connection<S::Res, S::Req> {}

impl<T: Connection<S::Res, S::Req>, S: Service> ClientConnection<S> for T {}

/// A server connection is a connection where requests are received and responses are sent
///
/// This is just a trait alias for TypedConnection<S::Req, S::Res>
pub trait ServerConnection<S: Service>: Connection<S::Req, S::Res> {}

impl<T: Connection<S::Req, S::Res>, S: Service> ServerConnection<S> for T {}
