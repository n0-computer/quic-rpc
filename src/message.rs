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

#[cfg(feature = "rpc-with-progress")]
mod rpc_with_progress {

    /// Interaction pattern for rpc messages that can report progress
    #[derive(Debug, Clone, Copy)]
    pub struct RpcWithProgress;

    impl InteractionPattern for RpcWithProgress {}

    /// A rpc message with progress updates
    ///
    /// This can be useful for long running operations where the client wants to
    /// display progress to the user.
    pub trait RpcWithProgressMsg<S: Service>: Msg<S> {
        /// The final response
        type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
        /// The self contained progress updates
        type Progress: Into<S::Res> + ConvertOrKeep<S::Res> + Send + 'static;
    }

    /// Helper trait to attempt a conversion and keep the original value if it fails
    ///
    /// To implement this you have to implement TryFrom for the type and the reference.
    /// This can be done with derive_more using #[derive(TryInto)].
    pub trait ConvertOrKeep<T>: TryFrom<T> + Sized {
        /// Convert the value or keep it if it can't be converted
        fn convert_or_keep(s: T) -> std::result::Result<Self, T>;
    }

    impl<T, U> ConvertOrKeep<U> for T
    where
        for<'a> &'a Self: TryFrom<&'a U>,
        Self: TryFrom<U>,
    {
        fn convert_or_keep(x: U) -> std::result::Result<Self, U> {
            let can_convert = (<&Self>::try_from(&x)).is_ok();
            if can_convert {
                Ok(Self::try_from(x)
                    .unwrap_or_else(|_| panic!("TryFrom inconsistent for byref and byval")))
            } else {
                Err(x)
            }
        }
    }
}
#[cfg(feature = "rpc-with-progress")]
pub use rpc_with_progress::{RpcWithProgress, RpcWithProgressMsg};
