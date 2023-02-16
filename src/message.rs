//! Traits to define the behaviour of messages for services
use crate::Service;
use std::fmt::Debug;

/// Defines interaction pattern for a message and a service.
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Pattern<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    /// The interaction pattern for this message with this service.
    type Pattern: InteractionPattern;
}

/// Defines the response type for a rpc message.
///
/// Since this is the most common interaction pattern, this also implements [Pattern] for you
/// automatically, with the interaction pattern set to [RpcPattern]. This is to reduce boilerplate
/// when defining rpc messages.
pub trait Rpc<S: Service>: Pattern<S, Pattern = RpcPattern> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

impl<T: Rpc<S>, S: Service> Pattern<S> for T {
    type Pattern = RpcPattern;
}

/// Defines update type and response type for a client streaming message.
pub trait ClientStreaming<S: Service>: Pattern<S, Pattern = ClientStreamingPattern> {
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
pub trait ServerStreaming<S: Service>: Pattern<S, Pattern = ServerStreamingPattern> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Defines update type and response type for a bidi streaming message.
pub trait BidiStreaming<S: Service>: Pattern<S, Pattern = BidiStreamingPattern> {
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
pub struct RpcPattern;
impl InteractionPattern for RpcPattern {}

/// Client streaming interaction pattern
///
/// After the initial request, the client can send updates, but there is only
/// one response.
#[derive(Debug, Clone, Copy)]
pub struct ClientStreamingPattern;
impl InteractionPattern for ClientStreamingPattern {}

/// Server streaming interaction pattern
///
/// After the initial request, the server can send a stream of responses.
#[derive(Debug, Clone, Copy)]
pub struct ServerStreamingPattern;
impl InteractionPattern for ServerStreamingPattern {}

/// Bidirectional streaming interaction pattern
///
/// After the initial request, the client can send updates and the server can
/// send responses.
#[derive(Debug, Clone, Copy)]
pub struct BidiStreamingPattern;
impl InteractionPattern for BidiStreamingPattern {}
