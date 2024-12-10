//! A streaming rpc system for transports that support multiple bidirectional
//! streams, such as QUIC and HTTP2.
//!
//! A lightweight memory transport is provided for cases where you want have
//! multiple cleanly separated substreams in the same process.
//!
//! For supported transports, see the [transport] module.
//!
//! # Motivation
//!
//! See the [README](https://github.com/n0-computer/quic-rpc/blob/main/README.md)
//!
//! # Example
//! ```
//! # async fn example() -> anyhow::Result<()> {
//! use derive_more::{From, TryInto};
//! use quic_rpc::{message::RpcMsg, RpcClient, RpcServer, Service};
//! use serde::{Deserialize, Serialize};
//!
//! // Define your messages
//! #[derive(Debug, Serialize, Deserialize)]
//! struct Ping;
//!
//! #[derive(Debug, Serialize, Deserialize)]
//! struct Pong;
//!
//! // Define your RPC service and its request/response types
//! #[derive(Debug, Clone)]
//! struct PingService;
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
//! impl Service for PingService {
//!     type Req = PingRequest;
//!     type Res = PingResponse;
//! }
//!
//! // Define interaction patterns for each request type
//! impl RpcMsg<PingService> for Ping {
//!     type Response = Pong;
//! }
//!
//! // create a transport channel, here a memory channel for testing
//! let (server, client) = quic_rpc::transport::flume::channel(1);
//!
//! // client side
//! // create the rpc client given the channel and the service type
//! let mut client = RpcClient::<PingService, _>::new(client);
//!
//! // call the service
//! let res = client.rpc(Ping).await?;
//!
//! // server side
//! // create the rpc server given the channel and the service type
//! let mut server = RpcServer::<PingService, _>::new(server);
//!
//! let handler = Handler;
//! loop {
//!     // accept connections
//!     let (msg, chan) = server.accept().await?.read_first().await?;
//!     // dispatch the message to the appropriate handler
//!     match msg {
//!         PingRequest::Ping(ping) => chan.rpc(ping, handler, Handler::ping).await?,
//!     }
//! }
//!
//! // the handler. For a more complex example, this would contain any state
//! // needed to handle the request.
//! #[derive(Debug, Clone, Copy)]
//! struct Handler;
//!
//! impl Handler {
//!     // the handle fn for a Ping request.
//!
//!     // The return type is the response type for the service.
//!     // Note that this must take self by value, not by reference.
//!     async fn ping(self, _req: Ping) -> Pong {
//!         Pong
//!     }
//! }
//! # Ok(())
//! # }
//! ```
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![cfg_attr(iroh_docsrs, feature(doc_cfg))]
use std::fmt::{Debug, Display};

use serde::{de::DeserializeOwned, Serialize};
pub mod client;
pub mod message;
pub mod server;
pub mod transport;
pub use client::RpcClient;
pub use server::RpcServer;
#[cfg(feature = "macros")]
mod macros;

pub mod pattern;

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
///
/// Instead we require them to implement `Into<anyhow::Error>`, which is available
/// both for any type that implements [std::error::Error] and anyhow itself.
pub trait RpcError: Debug + Display + Into<anyhow::Error> + Send + Sync + Unpin + 'static {}

impl<T> RpcError for T where T: Debug + Display + Into<anyhow::Error> + Send + Sync + Unpin + 'static
{}

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
/// Depending on the interaction type, you might need to implement traits that further
/// define details of the interaction.
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

/// A connector to a specific service
///
/// This is just a trait alias for a [`transport::Connector`] with the right types. It is used
/// to make it easier to specify the bounds of a connector that matches a specific
/// service.
pub trait Connector<S: Service>: transport::Connector<In = S::Res, Out = S::Req> {}

impl<T: transport::Connector<In = S::Res, Out = S::Req>, S: Service> Connector<S> for T {}

/// A listener for a specific service
///
/// This is just a trait alias for a [`transport::Listener`] with the right types. It is used
/// to make it easier to specify the bounds of a listener that matches a specific
/// service.
pub trait Listener<S: Service>: transport::Listener<In = S::Req, Out = S::Res> {}

impl<T: transport::Listener<In = S::Req, Out = S::Res>, S: Service> Listener<S> for T {}


#[cfg(feature = "flume-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
/// A struct that needs the flume transport feature flag
pub struct INeedFlumeTransport;

#[cfg(feature = "flume-transport")]
#[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
/// foo
pub mod flume_helpers {
    use super::{transport, RpcClient, RpcServer, Service};
    /// A flume listener for the given service
    #[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
    pub type FlumeListener<S> =
        transport::flume::FlumeListener<<S as Service>::Req, <S as Service>::Res>;

    /// A flume connector for the given service
    #[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
    pub type FlumeConnector<S> =
        transport::flume::FlumeConnector<<S as Service>::Res, <S as Service>::Req>;

    /// Create a pair of client and server using a flume channel
    #[cfg_attr(iroh_docsrs, doc(cfg(feature = "flume-transport")))]
    pub fn flume_channel<S: Service>(
        size: usize,
    ) -> (
        RpcServer<S, FlumeListener<S>>,
        RpcClient<S, FlumeConnector<S>>,
    ) {
        let (listener, connector) = transport::flume::channel(size);
        (RpcServer::new(listener), RpcClient::new(connector))
    }
}

#[cfg(feature = "flume-transport")]
pub use flume_helpers::*;

#[cfg(feature = "quinn-transport")]
mod quinn_helpers {

    use super::{transport, Service};
    /// A quinn listener for the given service
    pub type QuinnListener<S> =
        transport::quinn::QuinnListener<<S as Service>::Req, <S as Service>::Res>;

    /// A quinn connector for the given service
    pub type QuinnConnector<S> =
        transport::quinn::QuinnConnector<<S as Service>::Res, <S as Service>::Req>;

    #[cfg(feature = "test-utils")]
    /// Create a pair of client and server that are connected via a local network connection.
    ///
    /// This is useful for testing the quinn transport.
    pub fn quinn_channel<S: Service>() -> anyhow::Result<(
        super::RpcServer<S, QuinnListener<S>>,
        super::RpcClient<S, QuinnConnector<S>>,
    )> {
        use super::{RpcClient, RpcServer};
        let bind_addr: std::net::SocketAddr = ([0, 0, 0, 0], 0).into();
        let (server_endpoint, cert_der) = transport::quinn::make_server_endpoint(bind_addr)?;
        let addr = server_endpoint.local_addr()?;
        let server = QuinnListener::<S>::new(server_endpoint)?;
        let server = RpcServer::new(server);
        let client_endpoint = transport::quinn::make_client_endpoint(bind_addr, &[&cert_der])?;
        let client = QuinnConnector::<S>::new(client_endpoint, addr, "localhost".into());
        let client = RpcClient::new(client);
        Ok((server, client))
    }
}

#[cfg(feature = "quinn-transport")]
pub use quinn_helpers::*;
