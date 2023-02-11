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
use client::{ConnectionErrors, TypedConnection};
use futures::{Future, Sink, Stream};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fmt::{self, Debug, Display},
    net::SocketAddr,
    result,
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
    type ClientConnection<In: RpcMessage, Out: RpcMessage>: TypedConnection<In, Out>;
    /// The server connection type
    type ServerConnection<In: RpcMessage, Out: RpcMessage>: TypedConnection<In, Out>;
}

/// Defines a set of types for a kind of channel
///
/// Every distinct kind of channel has its own ChannelType. See e.g.
/// [crate::transport::MemChannelTypes].
pub trait ChannelTypes: Debug + Sized + Send + Sync + Unpin + Clone + 'static {
    // type ServerSource<In, Out>: TypedConnection<In, Out>;

    /// The sink used for sending either requests or responses on this channel
    type SendSink<M: RpcMessage>: Sink<M, Error = Self::SendError> + Send + Unpin + 'static;
    /// The stream used for receiving either requests or responses on this channel
    type RecvStream<M: RpcMessage>: Stream<Item = result::Result<M, Self::RecvError>>
        + Send
        + Unpin
        + 'static;
    /// Error you might get while sending messages to a sink
    type SendError: RpcError;
    /// Error you might get while receiving messages from a stream
    type RecvError: RpcError;
    /// Error you might get when opening a new connection to the server
    type OpenBiError: RpcError;
    /// Future returned by open_bi
    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage>: Future<
            Output = result::Result<(Self::SendSink<Out>, Self::RecvStream<In>), Self::OpenBiError>,
        > + Send
        + 'a
    where
        Self: 'a;

    /// Error you might get when waiting for new streams on the server side
    type AcceptBiError: RpcError;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage>: Future<
            Output = result::Result<
                (Self::SendSink<Out>, Self::RecvStream<In>),
                Self::AcceptBiError,
            >,
        > + Send
        + 'a
    where
        Self: 'a;

    /// Channel type
    type ClientChannel<In: RpcMessage, Out: RpcMessage>: ClientChannel<In, Out, Self>;

    /// Channel type
    type ServerChannel<In: RpcMessage, Out: RpcMessage>: ServerChannel<In, Out, Self>;
}

/// An abstract client channel with typed input and output
pub trait ClientChannel<In: RpcMessage, Out: RpcMessage, T: ChannelTypes>:
    Debug + Clone + Send + Sync + 'static
{
    /// Open a bidirectional stream
    fn open_bi(&self) -> T::OpenBiFuture<'_, In, Out>;
}

#[derive(Debug)]
struct ClientSource<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> {
    channel: T::ClientChannel<In, Out>,
}

impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> Clone for ClientSource<In, Out, T> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> ConnectionErrors
    for ClientSource<In, Out, T>
{
    type SendError = T::SendError;

    type RecvError = T::RecvError;

    type OpenError = T::OpenBiError;
}

// impl<In: RpcMessage, Out: RpcMessage, T: ChannelTypes> TypedConnection<In, Out>
//     for ClientSource<In, Out, T>
// {
//     type RecvStream = T::RecvStream<In>;

//     type SendSink = T::SendSink<Out>;

//     type SubstreamFut<'a> = T::OpenBiFuture<'a, In, Out>;

//     fn next(&self) -> Self::SubstreamFut<'_> {
//         self.channel.open_bi()
//     }
// }

/// An abstract server with typed input and output
pub trait ServerChannel<In: RpcMessage, Out: RpcMessage, T: ChannelTypes>:
    Debug + Clone + Send + Sync + 'static
{
    /// Accepts a bidirectional stream.
    ///
    /// This returns a future who's `Output` is a tuple of a sender sink and receiver stream
    /// to send and receive messages to and from the client respectively.  The sink and
    /// stream `Item`s are the whole `In` and `Out` messages, an [`RpcMessage`].
    fn accept_bi(&self) -> T::AcceptBiFuture<'_, In, Out>;

    /// The local addresses this server is bound to.
    ///
    /// This is useful for publicly facing addresses when you start the server with a random
    /// port, `:0` and let the kernel choose the real bind address.  This will return the
    /// address with the actual port used.
    fn local_addr(&self) -> &[LocalAddr];
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
