//! Memory transport implementation using [tokio::sync::mpsc]

use futures_lite::Stream;
use futures_sink::Sink;

use crate::{
    transport::{self, ConnectionErrors, LocalAddr},
    RpcMessage, Service,
};
use core::fmt;
use std::{error, fmt::Display, pin::Pin, result, sync::Arc, task::Poll};
use tokio::sync::{mpsc, Mutex};

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

/// A `tokio::sync::mpsc` based server endpoint.
///
/// Created using [connection].
pub struct ServerEndpoint<S: Service> {
    #[allow(clippy::type_complexity)]
    stream: Arc<Mutex<mpsc::Receiver<(SendSink<S::Res>, RecvStream<S::Req>)>>>,
}

impl<S: Service> Clone for ServerEndpoint<S> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
        }
    }
}

impl<S: Service> fmt::Debug for ServerEndpoint<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ServerEndpoint")
            .field("stream", &self.stream)
            .finish()
    }
}

impl<S: Service> ConnectionErrors for ServerEndpoint<S> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::AcceptBiError;
}

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

impl<S: Service> ConnectionCommon<S::Req, S::Res> for ServerEndpoint<S> {
    type SendSink = SendSink<S::Res>;
    type RecvStream = RecvStream<S::Req>;
}

impl<S: Service> transport::ServerEndpoint<S::Req, S::Res> for ServerEndpoint<S> {
    async fn accept_bi(&self) -> Result<(Self::SendSink, Self::RecvStream), AcceptBiError> {
        let (send, recv) = self
            .stream
            .lock()
            .await
            .recv()
            .await
            .ok_or(AcceptBiError::RemoteDropped)?;
        Ok((send, recv))
    }

    fn local_addr(&self) -> &[LocalAddr] {
        &[LocalAddr::Mem]
    }
}

impl<S: Service> ConnectionErrors for Connection<S> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = self::OpenBiError;
}

impl<S: Service> ConnectionCommon<S::Res, S::Req> for Connection<S> {
    type SendSink = SendSink<S::Req>;
    type RecvStream = RecvStream<S::Res>;
}

impl<S: Service> transport::Connection<S::Res, S::Req> for Connection<S> {
    async fn open_bi(&self) -> result::Result<Socket<S::Res, S::Req>, self::OpenBiError> {
        let (local_send, remote_recv) = mpsc::channel::<S::Req>(128);
        let (remote_send, local_recv) = mpsc::channel::<S::Res>(128);
        let remote_chan = (
            SendSink(tokio_util::sync::PollSender::new(remote_send)),
            RecvStream(tokio_stream::wrappers::ReceiverStream::new(remote_recv)),
        );
        let local_chan = (
            SendSink(tokio_util::sync::PollSender::new(local_send)),
            RecvStream(tokio_stream::wrappers::ReceiverStream::new(local_recv)),
        );
        self.sink
            .send(remote_chan)
            .await
            .map_err(|_| self::OpenBiError::RemoteDropped)?;
        Ok(local_chan)
    }
}

/// A tokio::sync::mpsc based connection to a server endpoint.
///
/// Created using [connection].
pub struct Connection<S: Service> {
    #[allow(clippy::type_complexity)]
    sink: mpsc::Sender<(SendSink<S::Res>, RecvStream<S::Req>)>,
}

impl<S: Service> Clone for Connection<S> {
    fn clone(&self) -> Self {
        Self {
            sink: self.sink.clone(),
        }
    }
}

impl<S: Service> fmt::Debug for Connection<S> {
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

/// Create a mpsc server endpoint and a connected mpsc client channel.
///
/// `buffer` the size of the buffer for each channel. Keep this at a low value to get backpressure
pub fn connection<S: Service>(buffer: usize) -> (ServerEndpoint<S>, Connection<S>) {
    let (sink, stream) = mpsc::channel(buffer);
    (
        ServerEndpoint {
            stream: Arc::new(Mutex::new(stream)),
        },
        Connection { sink },
    )
}
