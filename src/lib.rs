//! A streaming rpc system based on quic
//!
//! # Motivation
//!
//! See the [README](https://github.com/n0-computer/quic-rpc/blob/main/README.md)
//!
//! # Example
//! ```
//! # async fn example() -> anyhow::Result<()> {
//! use quic_rpc::{message::RpcMsg, Service, RpcClient};
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
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
use serde::{de::DeserializeOwned, Serialize};
use std::fmt::{Debug, Display};
use transport::{Connection, ServerEndpoint};
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
/// All errors have to be Send, Sync and 'static so they can be sent across threads.
/// They also have to be Debug and Display so they can be logged.
///
/// We don't require them to implement [std::error::Error] so we can use
/// anyhow::Error as an error type.
pub trait RpcError: Debug + Display + Send + Sync + Unpin + 'static {}

impl<T> RpcError for T where T: Debug + Display + Send + Sync + Unpin + 'static {}

/// A service
///
/// A service has request and response message types. These types have to be the
/// union of all possible request and response types for all interactions with
/// the service.
///
/// Usually you will define an enum for the request and response
/// type, and use the [derive_more](https://crates.io/crates/derive_more) crate to
/// define the conversions between the enum and the actual request and response types.
///
/// To make a message type usable as a request for a service, implement [message::Msg]
/// for it. This is how you define the interaction patterns for each request type.
///
/// A message type can be used for multiple services. E.g. you might have a
/// Status request that is understood by multiple services and returns a
/// standard status response.
pub trait Service: Send + Sync + Debug + Clone + 'static {
    /// Type of request messages
    type Req: RpcMessage;
    /// Type of response messages
    type Res: RpcMessage;
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
