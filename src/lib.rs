use futures::{Future, Sink, Stream};
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fmt::{Debug, Display},
    result,
};
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;
pub mod sugar;

/// requirements for a RPC message
///
/// Even when just using the mem transport, we require messages to be Serializable and Deserializable.
/// Likewise, even when using the quinn transport, we require messages to be Send.
///
/// This does not seem like a big restriction
pub trait RpcMessage: Serialize + DeserializeOwned + Send + Unpin + 'static {}

impl<T> RpcMessage for T where T: Serialize + DeserializeOwned + Send + Unpin + 'static {}

/// requirements for an internal error
///
/// All errors have to be Send and 'static so they can be sent across threads.
pub trait RpcError: Debug + Display + Send + Sync + Unpin + 'static {}

impl<T> RpcError for T where T: Debug + Display + Send + Sync + Unpin + 'static {}

/// A service
pub trait Service {
    type Req: RpcMessage;
    type Res: RpcMessage;
}

/// A module that defines a set of channel types
pub trait ChannelTypes: Debug + Sized + Send + Sync + Unpin + 'static {
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

    type Channel<In: RpcMessage, Out: RpcMessage>: crate::Channel<In, Out, Self>;
}

/// An abstract channel to a service
///
/// This assumes cheap streams, so every interaction uses a new stream.
///
/// Heavily inspired by quinn, but uses concrete Req and Res types instead of bytes. The reason for this is that
/// we want to be able to write a memory channel that does not serialize and deserialize.
pub trait Channel<In: RpcMessage, Out: RpcMessage, T: ChannelTypes>:
    Send + Sync + Clone + 'static
{
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> T::OpenBiFuture<'_, In, Out>;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> T::AcceptBiFuture<'_, In, Out>;
}
