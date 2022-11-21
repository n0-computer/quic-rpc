//! Traits to define the behaviour of messages for services
use crate::Service;
use std::fmt::Debug;

/// Defines interaction pattern, update type and return type for a RPC message
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Msg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    /// The type for request updates
    ///
    /// For a request that does not support updates, this can be safely set to any type, including
    /// the message type itself. Any update for such a request will result in an error.
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;

    /// The type for the response
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;

    /// The interaction pattern for this message with this service.
    type Pattern: InteractionPattern;
}

/// Shortcut to define just the return type for the very common RPC interaction pattern
pub trait RpcMsg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    /// The type for the response
    ///
    /// This is the only type that is required for the RPC interaction pattern.
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

impl<S: Service, T: RpcMsg<S>> Msg<S> for T {
    type Update = Self;

    type Response = T::Response;

    type Pattern = Rpc;
}

/// Trait defining interaction pattern.
///
/// Currently there are 4 patterns:
/// - `RPC`: 1 request, 1 response
/// - `ClientStreaming`: 1 request, stream of updates, 1 response
/// - `ServerStreaming`: 1 request, stream of responses
/// - `BidiStreaming`: 1 request, stream of updates, stream of responses
pub trait InteractionPattern: Debug + Clone + Send + Sync + 'static {}

/// RPC interaction pattern
#[derive(Debug, Clone, Copy)]
pub struct Rpc;
impl InteractionPattern for Rpc {}

/// Client streaming interaction pattern
#[derive(Debug, Clone, Copy)]
pub struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}

/// Server streaming interaction pattern
#[derive(Debug, Clone, Copy)]
pub struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}

/// Bidirectional streaming interaction pattern
#[derive(Debug, Clone, Copy)]
pub struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}
