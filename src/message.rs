//! Service definition
//!
//! Traits to define the behaviour of messages for services
use crate::Service;
use std::fmt::Debug;

/// Declares the interaction pattern for a message and a service.
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Msg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    /// The interaction pattern for this message with this service.
    type Pattern: InteractionPattern;
}

/// Defines the response type for a rpc message.
///
/// Since this is the most common interaction pattern, this also implements [Msg] for you
/// automatically, with the interaction pattern set to [Rpc]. This is to reduce boilerplate
/// when defining rpc messages.
pub trait RpcMsg<S: Service>: Msg<S, Pattern = Rpc> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// We can only do this for one trait, so we do it for RpcMsg since it is the most common
impl<T: RpcMsg<S>, S: Service> Msg<S> for T {
    type Pattern = Rpc;
}

/// Defines update type and response type for a client streaming message.
pub trait ClientStreamingMsg<S: Service>: Msg<S, Pattern = ClientStreaming> {
    /// The type for request updates
    ///
    /// For a request that does not support updates, this can be safely set to any type, including
    /// the message type itself. Any update for such a request will result in an error.
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;

    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Defines response type for a server streaming message.
pub trait ServerStreamingMsg<S: Service>: Msg<S, Pattern = ServerStreaming> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Defines update type and response type for a bidi streaming message.
pub trait BidiStreamingMsg<S: Service>: Msg<S, Pattern = BidiStreaming> {
    /// The type for request updates
    ///
    /// For a request that does not support updates, this can be safely set to any type, including
    /// the message type itself. Any update for such a request will result in an error.
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;

    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Trait defining interaction pattern.
///
/// Currently there are 4 patterns:
/// - [Rpc]: 1 request, 1 response
/// - [ClientStreaming]: 1 request, stream of updates, 1 response
/// - [ServerStreaming]: 1 request, stream of responses
/// - [BidiStreaming]: 1 request, stream of updates, stream of responses
///
/// You could define your own interaction patterns such as OneWay.
pub trait InteractionPattern: Debug + Clone + Send + Sync + 'static {}

/// Rpc interaction pattern
///
/// There is only one request and one response.
#[derive(Debug, Clone, Copy)]
pub struct Rpc;
impl InteractionPattern for Rpc {}

/// Client streaming interaction pattern
///
/// After the initial request, the client can send updates, but there is only
/// one response.
#[derive(Debug, Clone, Copy)]
pub struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}

/// Server streaming interaction pattern
///
/// After the initial request, the server can send a stream of responses.
#[derive(Debug, Clone, Copy)]
pub struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}

/// Bidirectional streaming interaction pattern
///
/// After the initial request, the client can send updates and the server can
/// send responses.
#[derive(Debug, Clone, Copy)]
pub struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}
