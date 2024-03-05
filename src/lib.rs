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
//! use quic_rpc::{message::RpcMsg, Service, RpcClient, RpcServer};
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
//!   type Req = PingRequest;
//!   type Res = PingResponse;
//! }
//!
//! // Define interaction patterns for each request type
//! impl RpcMsg<PingService> for Ping {
//!   type Response = Pong;
//! }
//!
//! // create a transport channel, here a memory channel for testing
//! let (server, client) = quic_rpc::transport::flume::connection::<PingRequest, PingResponse>(1);
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
//!   // accept connections
//!   let (msg, chan) = server.accept().await?;
//!   // dispatch the message to the appropriate handler
//!   match msg {
//!     PingRequest::Ping(ping) => chan.rpc(ping, handler, Handler::ping).await?,
//!   }
//! }
//!
//! // the handler. For a more complex example, this would contain any state
//! // needed to handle the request.
//! #[derive(Debug, Clone, Copy)]
//! struct Handler;
//!
//! impl Handler {
//!   // the handle fn for a Ping request.
//!
//!   // The return type is the response type for the service.
//!   // Note that this must take self by value, not by reference.
//!   async fn ping(self, _req: Ping) -> Pong {
//!     Pong
//!   }
//! }
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
#[cfg(feature = "macros")]
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

// pub trait MappedService2<S, Req, Res>: Service<Req, Res> {
//     type Req
//
// }
///
// pub trait ServiceWrapperFor<S>: Service {}
//
// impl<S0, S2> ServiceWrapperFor<S2> for S0
// where
//     S0: Service,
//     S2: Service,
//     S0::Req: From<S2::Req>,
//     S0::Res: From<S0::Res>,
// {
// }
// //
// ///
// pub trait FromService<S: Service>: Service {
//     ///
//     fn req_from(req: <Self as Service>::Req) -> S::Req;
//     ///
//     fn res_from(res: <Self as Service>::Res) -> S::Res;
// }
//
// impl<S0, S2> FromService<S0> for S2
// where
//     S0: Service,
//     S2: Service,
//     S2::Req: Into<S0::Req> + Send + 'static,
//     S2::Res: Into<S0::Res> + Send + 'static,
// {
//     fn req_from(req: <Self as Service>::Req) -> S0::Req {
//         req.into()
//     }
//     fn res_from(res: <Self as Service>::Res) -> S0::Res {
//         res.into()
//     }
// }

///
pub trait IntoService<S: Service>: Service {
    ///
    fn outer_req_from(req: S::Req) -> Self::Req;
    ///
    fn outer_res_from(res: S::Res) -> Self::Res;
    ///
    fn try_inner_req_from(req: Self::Req) -> Result<S::Req, ()>;
    ///
    fn try_inner_res_from(res: Self::Res) -> Result<S::Res, ()>;
}

impl<S0, S2> IntoService<S2> for S0
where
    S0: Service,
    S2: Service,
    S2::Req: Into<S0::Req> + Send + 'static,
    S2::Res: Into<S0::Res> + Send + 'static,

    S2::Req: TryFrom<S0::Req> + Send + 'static,
    S2::Res: TryFrom<S0::Res> + Send + 'static,
{
    fn outer_req_from(req: S2::Req) -> S0::Req {
        req.into()
    }
    fn outer_res_from(res: S2::Res) -> S0::Res {
        res.into()
    }

    ///
    fn try_inner_req_from(req: S0::Req) -> Result<S2::Req, ()> {
        req.try_into().map_err(|_| ())
    }
    ///
    fn try_inner_res_from(res: Self::Res) -> Result<S2::Res, ()> {
        res.try_into().map_err(|_| ())
    }

    // fn try_req_into(req: S2::Req) -> <Self as Service>::Req {
    //     req.into()
    // }
    // fn try_res_from(res: S2::Res) -> <Self as Service>::Res {
    //     res.into()
    // }
}

// ///
// pub trait IntoService<S: Service>: Service {
//     fn req_into(req: <Self as Service>::Req) -> S::Req;
//     fn res_into(res: <Self as Service>::Res) -> S::Res;
// }
//
// impl<S0, S2> IntoService<S2> for S0
// where
//     S0: Service,
//     S2: Service,
//     S2::Req: Into<S0::Req> + Send + 'static,
//     S2::Res: Into<S0::Res> + Send + 'static,
// {
//     fn req_into(req: S2::Req) -> S0::Req {
//         req.into()
//     }
//     fn res_into(res: S2::Res) -> S0::Res {
//         res.into()
//     }
// }

// impl<S0, S2> IntoService<S2> for S0 where S0: Service, S2: FromService<S0> {
//     fn req_into(req: <S2 as Service>::Req) -> <S0 as Service>::Req {
//         <S0 as FromService<S2>>::req_from(req)
//     }
//
//     fn res_into(res: <S2 as Service>::Res) -> <S0 as Service>::Res {
//         <S0 as FromService<S2>>::res_from(res)
//     }
// }

// impl<S2, S0> MappedService<S0> for S2
// where
//     S0: Service,
//     S2: Service,
//     // <T as Service>::Req: From<S::Req>,
//     // <T as Service>::Res: From<S::Res>,
//     // <S0 as Service>::Req: From<S2::Req>,
//     // <S0 as Service>::Res: From<S2::Res>,
//     // <S2 as Service>::Req: From<S0::Req>,
//     // <S2 as Service>::Res: From<S0::Res>,
//         S2::Req: Into<S0::Req> + Send + 'static,
//         S2::Res: Into<S0::Res> + Send + 'static,
// {
// }
// S0: quic_rpc::Service,
// S0::Req: From<Request> + Send + 'static,
// S0::Res: From<Response> + Send + 'static,

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
