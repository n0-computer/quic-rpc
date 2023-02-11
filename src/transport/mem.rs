//! Memory channel implementation
//!
//! This is currently based on [flume], but since no flume types are exposed it can be changed to another
//! mpmc channel implementation, like [crossbeam].
//!
//! [flume]: https://docs.rs/flume/
//! [crossbeam]: https://docs.rs/crossbeam/
use crate::{ChannelTypes2, Connection, ConnectionErrors, RpcMessage};
use core::fmt;
use futures::{future::BoxFuture, FutureExt, Sink, SinkExt, Stream, StreamExt};
use std::{error, fmt::Display, pin::Pin, result, task::Poll};

/// Error when receiving from a channel
///
/// This type has zero inhabitants, so it is always safe to unwrap a result with this error type.
#[derive(Debug)]
pub enum RecvError {}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub struct SendSink<T: RpcMessage>(flume::r#async::SendSink<'static, T>);

impl<T: RpcMessage> Sink<T> for SendSink<T> {
    type Error = self::SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_ready_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn start_send(mut self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        self.0
            .start_send_unpin(item)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_flush_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_close_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }
}
pub struct RecvStream<T: RpcMessage>(flume::r#async::RecvStream<'static, T>);

impl<T: RpcMessage> Stream for RecvStream<T> {
    type Item = result::Result<T, self::RecvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            Poll::Ready(Some(v)) => Poll::Ready(Some(Ok(v))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl error::Error for RecvError {}

pub struct MemChannelTypes;

impl ChannelTypes2 for MemChannelTypes {
    type ClientConnection<In: RpcMessage, Out: RpcMessage> = MemClientChannel<In, Out>;
    type ServerConnection<In: RpcMessage, Out: RpcMessage> = MemServerChannel<In, Out>;
}

/// A mem channel
pub struct MemServerChannel<In: RpcMessage, Out: RpcMessage> {
    stream: flume::Receiver<(SendSink<Out>, RecvStream<In>)>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for MemServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for MemServerChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerChannel")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for MemServerChannel<In, Out> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::AcceptBiError;
}

impl<In: RpcMessage, Out: RpcMessage> Connection<In, Out> for MemServerChannel<In, Out> {
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;

    type NextFut<'a> =
        BoxFuture<'a, result::Result<(Self::SendSink, Self::RecvStream), AcceptBiError>>;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let channel = self
                .stream
                .recv_async()
                .await
                .map_err(|_| AcceptBiError::RemoteDropped)?;
            Ok(channel)
        }
        .boxed()
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for MemClientChannel<In, Out> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::OpenBiError;
}

impl<In: RpcMessage, Out: RpcMessage> Connection<In, Out> for MemClientChannel<In, Out> {
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;

    type NextFut<'a> =
        BoxFuture<'a, result::Result<(Self::SendSink, Self::RecvStream), OpenBiError>>;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let (local_send, remote_recv) = flume::bounded::<Out>(128);
            let (remote_send, local_recv) = flume::bounded::<In>(128);
            let remote_chan = (
                SendSink(remote_send.into_sink()),
                RecvStream(remote_recv.into_stream()),
            );
            let local_chan = (
                SendSink(local_send.into_sink()),
                RecvStream(local_recv.into_stream()),
            );
            self.sink.send_async(remote_chan).await.unwrap();
            Ok(local_chan)
        }
        .boxed()
    }
}

/// A mem channel
pub struct MemClientChannel<In: RpcMessage, Out: RpcMessage> {
    sink: flume::Sender<(SendSink<In>, RecvStream<Out>)>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for MemClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for MemClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("sink", &self.sink)
            .finish()
    }
}

/// AcceptBiError for mem channels.
///
/// There is not much that can go wrong with mem channels.
#[derive(Debug)]
pub enum AcceptBiError {
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl fmt::Display for AcceptBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for AcceptBiError {}

/// SendError for mem channels.
///
/// There is not much that can go wrong with mem channels.
#[derive(Debug)]
pub enum SendError {
    /// Receiver was dropped
    ReceiverDropped,
}

impl Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for SendError {}

/// OpenBiError for mem channels.
#[derive(Debug)]
pub enum OpenBiError {
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl Display for OpenBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for OpenBiError {}

/// CreateChannelError for mem channels.
///
/// You can always create a mem channel, so there is no possible error.
/// Nevertheless we need a type for it.
#[derive(Debug, Clone, Copy)]
pub enum CreateChannelError {}

impl Display for CreateChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CreateChannelError {}

/// Create a channel pair (server, client) for mem channels
///
/// `buffer` the size of the buffer for each channel. Keep this at a low value to get backpressure
pub fn connection<Req: RpcMessage, Res: RpcMessage>(
    buffer: usize,
) -> (MemServerChannel<Req, Res>, MemClientChannel<Res, Req>) {
    let (sink, stream) = flume::bounded(buffer);
    (MemServerChannel { stream }, MemClientChannel { sink })
}
