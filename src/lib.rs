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
//! let mut client = RpcClient::<PingService, MemChannelTypes>::new(client);
//!
//! // call the service
//! let res = client.rpc(Ping).await?;
//! # Ok(())
//! # }
//! ```
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
use futures::{Future, Sink, Stream};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fmt::{Debug, Display},
    result,
};
pub mod client;
pub mod macros;
pub mod message;
pub mod server;
pub mod transport;
pub use client::RpcClient;
pub use server::RpcServer;

/// requirements for a RPC message
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

/// requirements for an internal error
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

/// Defines a set of types for a kind of channel
///
/// Every distinct kind of channel has its own ChannelType. See e.g.
/// [crate::transport::MemChannelTypes].
pub trait ChannelTypes: Debug + Sized + Send + Sync + Unpin + Clone + 'static {
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
    type ClientChannel<In: RpcMessage, Out: RpcMessage>: crate::ClientChannel<In, Out, Self>;

    /// Channel type
    type ServerChannel<In: RpcMessage, Out: RpcMessage>: crate::ServerChannel<In, Out, Self>;
}

/// An abstract client channel with typed input and output
pub trait ClientChannel<In: RpcMessage, Out: RpcMessage, T: ChannelTypes>:
    Debug + Clone + Send + Sync + 'static
{
    /// Open a bidirectional stream
    fn open_bi(&self) -> T::OpenBiFuture<'_, In, Out>;
}

/// An abstract server with typed input and output
pub trait ServerChannel<In: RpcMessage, Out: RpcMessage, T: ChannelTypes>:
    Debug + Clone + Send + Sync + 'static
{
    /// Accept a bidirectional stream
    fn accept_bi(&self) -> T::AcceptBiFuture<'_, In, Out>;
}
