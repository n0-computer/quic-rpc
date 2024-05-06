//! Memory transport implementation using [flume]
//!
//! [flume]: https://docs.rs/flume/
use futures_lite::{Future, Stream};
use futures_sink::Sink;

use crate::{
    transport::{Connection, ConnectionErrors, LocalAddr, ServerEndpoint},
    RpcMessage, Service,
};
use core::fmt;
use std::{error, fmt::Display, marker::PhantomData, pin::Pin, result, task::Poll};

use super::ConnectionCommon;

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
pub struct SendSink<T: RpcMessage>(flume::r#async::SendSink<'static, T>);

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
pub struct RecvStream<T: RpcMessage>(flume::r#async::RecvStream<'static, T>);

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

/// A flume based server endpoint.
///
/// Created using [connection].
pub struct FlumeServerEndpoint<S: Service> {
    stream: flume::Receiver<(SendSink<S::Res>, RecvStream<S::Req>)>,
}

impl<S: Service> Clone for FlumeServerEndpoint<S> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
        }
    }
}

impl<S: Service> fmt::Debug for FlumeServerEndpoint<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlumeServerEndpoint")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<S: Service> ConnectionErrors for FlumeServerEndpoint<S> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::AcceptBiError;
}

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

/// Future returned by [FlumeConnection::open_bi]
pub struct OpenBiFuture<In: RpcMessage, Out: RpcMessage> {
    inner: flume::r#async::SendFut<'static, Socket<Out, In>>,
    res: Option<Socket<In, Out>>,
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for OpenBiFuture<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBiFuture").finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> OpenBiFuture<In, Out> {
    fn new(inner: flume::r#async::SendFut<'static, Socket<Out, In>>, res: Socket<In, Out>) -> Self {
        Self {
            inner,
            res: Some(res),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<In, Out> {
    type Output = result::Result<Socket<In, Out>, self::OpenBiError>;

    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match Pin::new(&mut self.inner).poll(cx) {
            Poll::Ready(Ok(())) => self
                .res
                .take()
                .map(|x| Poll::Ready(Ok(x)))
                .unwrap_or(Poll::Pending),
            Poll::Ready(Err(_)) => Poll::Ready(Err(self::OpenBiError::RemoteDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Future returned by [FlumeServerEndpoint::accept_bi]
pub struct AcceptBiFuture<In: RpcMessage, Out: RpcMessage> {
    wrapped: flume::r#async::RecvFut<'static, (SendSink<Out>, RecvStream<In>)>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for AcceptBiFuture<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptBiFuture").finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Future for AcceptBiFuture<In, Out> {
    type Output = result::Result<(SendSink<Out>, RecvStream<In>), AcceptBiError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.wrapped).poll(cx) {
            Poll::Ready(Ok((send, recv))) => Poll::Ready(Ok((send, recv))),
            Poll::Ready(Err(_)) => Poll::Ready(Err(AcceptBiError::RemoteDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<S: Service> ConnectionCommon<S::Req, S::Res> for FlumeServerEndpoint<S> {
    type SendSink = SendSink<S::Res>;
    type RecvStream = RecvStream<S::Req>;
}

impl<S: Service> ServerEndpoint<S::Req, S::Res> for FlumeServerEndpoint<S> {
    type AcceptBiFut = AcceptBiFuture<S::Req, S::Res>;

    fn accept_bi(&self) -> Self::AcceptBiFut {
        AcceptBiFuture {
            wrapped: self.stream.clone().into_recv_async(),
            _p: PhantomData,
        }
    }

    fn local_addr(&self) -> &[LocalAddr] {
        &[LocalAddr::Mem]
    }
}

impl<S: Service> ConnectionErrors for FlumeConnection<S> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::OpenBiError;
}

impl<S: Service> ConnectionCommon<S::Res, S::Req> for FlumeConnection<S> {
    type SendSink = SendSink<S::Req>;
    type RecvStream = RecvStream<S::Res>;
}

impl<S: Service> Connection<S::Res, S::Req> for FlumeConnection<S> {
    type OpenBiFut = OpenBiFuture<S::Res, S::Req>;

    fn open_bi(&self) -> Self::OpenBiFut {
        let (local_send, remote_recv) = flume::bounded::<S::Req>(128);
        let (remote_send, local_recv) = flume::bounded::<S::Res>(128);
        let remote_chan = (
            SendSink(remote_send.into_sink()),
            RecvStream(remote_recv.into_stream()),
        );
        let local_chan = (
            SendSink(local_send.into_sink()),
            RecvStream(local_recv.into_stream()),
        );
        OpenBiFuture::new(self.sink.clone().into_send_async(remote_chan), local_chan)
    }
}

/// A flume based connection to a server endpoint.
///
/// Created using [connection].
pub struct FlumeConnection<S: Service> {
    sink: flume::Sender<(SendSink<S::Res>, RecvStream<S::Req>)>,
}

impl<S: Service> Clone for FlumeConnection<S> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<S: Service> fmt::Debug for FlumeConnection<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FlumeClientChannel")
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

/// Create a flume server endpoint and a connected flume client channel.
///
/// `buffer` the size of the buffer for each channel. Keep this at a low value to get backpressure
pub fn connection<S: Service>(buffer: usize) -> (FlumeServerEndpoint<S>, FlumeConnection<S>) {
    let (sink, stream) = flume::bounded(buffer);
    (FlumeServerEndpoint { stream }, FlumeConnection { sink })
}
