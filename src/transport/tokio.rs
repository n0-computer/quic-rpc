//! Memory transport implementation using [tokio::sync::mpsc::channel].
use core::fmt;
use std::{error, fmt::Display, pin::Pin, result, task::Poll};

use futures_lite::{Future, Stream};
use futures_sink::Sink;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::PollSender;

use super::StreamTypes;
use crate::{
    transport::{ConnectionErrors, Connector, Listener, LocalAddr},
    RpcMessage,
};

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

/// Sink for memory channels
pub struct SendSink<T: RpcMessage>(pub(crate) tokio_util::sync::PollSender<T>);

impl<T: RpcMessage> fmt::Debug for SendSink<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendSink").finish()
    }
}

impl<T: RpcMessage> Sink<T> for SendSink<T> {
    type Error = self::SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_ready(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn start_send(mut self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        Pin::new(&mut self.0)
            .start_send(item)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_flush(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0)
            .poll_close(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }
}

/// Stream for memory channels
pub struct RecvStream<T: RpcMessage>(pub(crate) tokio_stream::wrappers::ReceiverStream<T>);

impl<T: RpcMessage> fmt::Debug for RecvStream<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream").finish()
    }
}

impl<T: RpcMessage> Stream for RecvStream<T> {
    type Item = result::Result<T, self::RecvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match Pin::new(&mut self.0).poll_next(cx) {
            Poll::Ready(Some(v)) => Poll::Ready(Some(Ok(v))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl error::Error for RecvError {}

/// A flume based listener.
///
/// Created using [channel].
pub struct MemListener<In: RpcMessage, Out: RpcMessage> {
    #[allow(clippy::type_complexity)]
    stream: tokio::sync::Mutex<tokio::sync::mpsc::Receiver<(SendSink<Out>, RecvStream<In>)>>,
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for MemListener<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlumeListener")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for MemListener<In, Out> {
    type SendError = self::SendError;
    type RecvError = self::RecvError;
    type OpenError = self::OpenError;
    type AcceptError = self::AcceptError;
}

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

/// Future returned by [FlumeConnection::open]
pub struct OpenFuture<In: RpcMessage, Out: RpcMessage> {
    inner: PollSender<Socket<Out, In>>,
    send: Option<Socket<Out, In>>,
    res: Option<Socket<In, Out>>,
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for OpenFuture<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBiFuture").finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> OpenFuture<In, Out> {
    fn new(
        inner: PollSender<Socket<Out, In>>,
        send: Socket<Out, In>,
        res: Socket<In, Out>,
    ) -> Self {
        Self {
            inner,
            send: Some(send),
            res: Some(res),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Future for OpenFuture<In, Out> {
    type Output = result::Result<Socket<In, Out>, self::OpenError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match Pin::new(&mut self.inner).poll_reserve(cx) {
            Poll::Ready(Ok(())) => {
                let Some(item) = self.send.take() else {
                    return Poll::Pending;
                };
                let Ok(_) = self.inner.send_item(item) else {
                    return Poll::Ready(Err(self::OpenError::RemoteDropped));
                };
                self.res
                    .take()
                    .map(|x| Poll::Ready(Ok(x)))
                    .unwrap_or(Poll::Pending)
            }
            Poll::Ready(Err(_)) => Poll::Ready(Err(self::OpenError::RemoteDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for MemListener<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Listener for MemListener<In, Out> {
    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), AcceptError> {
        let mut stream = self.stream.lock().await;
        match stream.recv().await {
            Some((send, recv)) => Ok((send, recv)),
            None => Err(AcceptError::RemoteDropped),
        }
    }

    fn local_addr(&self) -> &[LocalAddr] {
        &[LocalAddr::Mem]
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for MemConnector<In, Out> {
    type SendError = self::SendError;
    type RecvError = self::RecvError;
    type OpenError = self::OpenError;
    type AcceptError = self::AcceptError;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for MemConnector<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Connector for MemConnector<In, Out> {
    #[allow(refining_impl_trait)]
    fn open(&self) -> OpenFuture<In, Out> {
        let (local_send, remote_recv) = tokio::sync::mpsc::channel::<Out>(128);
        let (remote_send, local_recv) = tokio::sync::mpsc::channel::<In>(128);
        let remote_chan = (
            SendSink(PollSender::new(remote_send)),
            RecvStream(ReceiverStream::new(remote_recv)),
        );
        let local_chan = (
            SendSink(PollSender::new(local_send)),
            RecvStream(ReceiverStream::new(local_recv)),
        );
        let sender = PollSender::new(self.sink.clone());
        OpenFuture::new(sender, remote_chan, local_chan)
    }
}

/// A flume based connector.
///
/// Created using [channel].
pub struct MemConnector<In: RpcMessage, Out: RpcMessage> {
    #[allow(clippy::type_complexity)]
    sink: tokio::sync::mpsc::Sender<(SendSink<In>, RecvStream<Out>)>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for MemConnector<In, Out> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for MemConnector<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemConnector")
            .field("sink", &self.sink)
            .finish()
    }
}

/// AcceptError for mem channels.
///
/// There is not much that can go wrong with mem channels.
#[derive(Debug)]
pub enum AcceptError {
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl fmt::Display for AcceptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for AcceptError {}

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

/// OpenError for mem channels.
#[derive(Debug)]
pub enum OpenError {
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for OpenError {}

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

/// Create a flume listener and a connected flume connector.
///
/// `buffer` the size of the buffer for each channel. Keep this at a low value to get backpressure
pub fn channel<Req: RpcMessage, Res: RpcMessage>(
    buffer: usize,
) -> (MemListener<Req, Res>, MemConnector<Res, Req>) {
    let (sink, stream) = tokio::sync::mpsc::channel(buffer);
    let stream = tokio::sync::Mutex::new(stream);
    (MemListener { stream }, MemConnector { sink })
}
