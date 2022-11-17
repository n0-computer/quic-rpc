use futures::{Future, Sink, Stream};
use serde::{de::DeserializeOwned, Serialize};
use std::{fmt::Debug, result};
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;
pub mod sugar;

/// A service
pub trait Service {
    type Req: Serialize + DeserializeOwned + Send + Unpin + 'static;
    type Res: Serialize + DeserializeOwned + Send + Unpin + 'static;
}

/// An abstract channel to a service
///
/// This assumes cheap streams, so every interaction uses a new stream.
///
/// Heavily inspired by quinn, but uses concrete Req and Res types instead of bytes. The reason for this is that
/// we want to be able to write a memory channel that does not serialize and deserialize.
pub trait Channel<In: Serialize + DeserializeOwned, Out: Serialize + DeserializeOwned + Unpin> {
    /// The sink used for sending either requests or responses on this channel
    type SendSink<M: Serialize + Unpin>: Sink<M, Error = Self::SendError>;
    /// The stream used for receiving either requests or responses on this channel
    type RecvStream<M: DeserializeOwned>: Stream<Item = result::Result<M, Self::RecvError>>;
    /// Error you might get while sending messages to a sink
    type SendError: Debug;
    /// Error you might get while receiving messages from a stream
    type RecvError: Debug;
    /// Error you might get when opening a new connection to the server
    type OpenBiError: Debug;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<
            Output = result::Result<(Self::RecvStream<In>, Self::SendSink<Out>), Self::OpenBiError>,
        > + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams on the server side
    type AcceptBiError: Debug;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<
            Output = result::Result<
                (Self::RecvStream<In>, Self::SendSink<Out>),
                Self::AcceptBiError,
            >,
        > + 'a
    where
        Self: 'a;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}
