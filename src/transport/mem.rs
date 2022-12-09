//! Memory channel implementation
//!
//! This is currently based on [flume], but since no flume types are exposed it can be changed to another
//! mpmc channel implementation, like [crossbeam].
//!
//! [flume]: https://docs.rs/flume/
//! [crossbeam]: https://docs.rs/crossbeam/
use crate::RpcMessage;
use core::fmt;
use futures::{Future, FutureExt, Sink, SinkExt, StreamExt};
use pin_project::pin_project;
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

impl error::Error for RecvError {}

/// RecvStream for mem channels
pub struct RecvStream<Res: RpcMessage>(pub(crate) flume::r#async::RecvStream<'static, Res>);

impl<In: RpcMessage> Clone for RecvStream<In> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<Res: RpcMessage> futures::Stream for RecvStream<Res> {
    type Item = Result<Res, RecvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

/// A mem channel
pub struct ServerChannel<In: RpcMessage, Out: RpcMessage> {
    stream: flume::Receiver<Socket<In, Out>>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ServerChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerChannel")
            .field("stream", &self.stream)
            .finish()
    }
}
/// A mem channel
pub struct ClientChannel<In: RpcMessage, Out: RpcMessage> {
    sink: flume::Sender<Socket<Out, In>>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ClientChannel<In, Out> {
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

/// Future returned by accept_bi
#[pin_project]
pub struct OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> {
    #[pin]
    inner: flume::r#async::SendFut<'a, Socket<Out, In>>,
    res: Option<Socket<In, Out>>,
}

impl<'a, In: RpcMessage, Out: RpcMessage> OpenBiFuture<'a, In, Out> {
    fn new(inner: flume::r#async::SendFut<'a, Socket<Out, In>>, res: Socket<In, Out>) -> Self {
        Self {
            inner,
            res: Some(res),
        }
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<Socket<In, Out>, self::OpenBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        match this.inner.poll_unpin(cx) {
            Poll::Ready(Ok(())) => {
                println!("got rid of channel!");
                this.res
                    .take()
                    .map(|x| Poll::Ready(Ok(x)))
                    .unwrap_or(Poll::Pending)
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(self::OpenBiError::RemoteDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future returned by accept_bi
pub struct AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage>(
    flume::r#async::RecvFut<'a, Socket<In, Out>>,
);

impl<'a, In: RpcMessage, Out: RpcMessage> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<Socket<In, Out>, AcceptBiError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.0.poll_unpin(cx) {
            Poll::Ready(Ok(socket)) => Poll::Ready(Ok(socket)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(AcceptBiError::RemoteDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// SendSink for mem channels
pub struct SendSink<Out: RpcMessage>(pub(crate) flume::r#async::SendSink<'static, Out>);

impl<Out: RpcMessage> Sink<Out> for SendSink<Out> {
    type Error = SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_ready_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
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

/// Types for mem channels.
#[derive(Debug, Clone, Copy)]
pub struct ChannelTypes;

impl crate::ChannelTypes for ChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::ClientChannel<In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::ServerChannel<In, Out>;
}

impl<In: RpcMessage, Out: RpcMessage> crate::ClientChannel<In, Out, ChannelTypes>
    for ClientChannel<In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        let (local_send, remote_recv) = flume::bounded::<Out>(128);
        let (remote_send, local_recv) = flume::bounded::<In>(128);
        let remote_recv = RecvStream(remote_recv.into_stream());
        let local_recv = RecvStream(local_recv.into_stream());
        let remote_send = SendSink(remote_send.into_sink());
        let local_send = SendSink(local_send.into_sink());
        let inner = self.sink.send_async((remote_send, remote_recv));
        OpenBiFuture::new(inner, (local_send, local_recv))
    }
}

impl<In: RpcMessage, Out: RpcMessage> crate::ServerChannel<In, Out, ChannelTypes>
    for ServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(self.stream.recv_async())
    }
}

/// Create a channel pair (server, client) for mem channels
///
/// `buffer` the size of the buffer for each channel. Keep this at a low value to get backpressure
pub fn connection<Req: RpcMessage, Res: RpcMessage>(
    buffer: usize,
) -> (ServerChannel<Req, Res>, ClientChannel<Res, Req>) {
    let (sink, stream) = flume::bounded::<Socket<Req, Res>>(buffer);
    (ServerChannel { stream }, ClientChannel { sink })
}
