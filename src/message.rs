//! Service definition
//!
//! Traits to define the behaviour of messages for services
use crate::Service;
use std::fmt::Debug;

pub use crate::pattern::bidi_streaming::{BidiStreaming, BidiStreamingMsg};
pub use crate::pattern::client_streaming::{ClientStreaming, ClientStreamingMsg};
pub use crate::pattern::rpc::{Rpc, RpcMsg};
pub use crate::pattern::server_streaming::{ServerStreaming, ServerStreamingMsg};

/// Declares the interaction pattern for a message and a service.
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Msg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    /// The interaction pattern for this message with this service.
    type Pattern: InteractionPattern;
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
