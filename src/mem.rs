use crate::{ChannelTypes, RpcMessage};
use core::fmt;
use futures::{Future, FutureExt, Sink, SinkExt, StreamExt};
use pin_project::pin_project;
use std::{fmt::Display, pin::Pin, result, task::Poll};

#[derive(Debug)]
pub enum NoError {}

impl fmt::Display for NoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for NoError {}

pub struct RecvStream<Res: RpcMessage>(flume::r#async::RecvStream<'static, Res>);

impl<Res: RpcMessage> futures::Stream for RecvStream<Res> {
    type Item = Result<Res, NoError>;

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

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

pub struct Channel<In: RpcMessage, Out: RpcMessage> {
    stream: flume::Receiver<Socket<In, Out>>,
    sink: flume::Sender<Socket<Out, In>>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for Channel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            stream: self.stream.clone(),
            sink: self.sink.clone(),
        }
    }
}

#[derive(Debug)]
pub enum AcceptBiError {
    SenderDropped,
}

impl fmt::Display for AcceptBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for AcceptBiError {}

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
            Poll::Ready(Ok(())) => this
                .res
                .take()
                .map(|x| Poll::Ready(Ok(x)))
                .unwrap_or(Poll::Pending),
            Poll::Ready(Err(_)) => Poll::Ready(Err(self::OpenBiError::Disconnected)),
            Poll::Pending => Poll::Pending,
        }
    }
}

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
            Poll::Ready(Err(_)) => Poll::Ready(Err(AcceptBiError::SenderDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub struct SendSink<Out: RpcMessage>(flume::r#async::SendSink<'static, Out>);

impl<Out: RpcMessage> Sink<Out> for SendSink<Out> {
    type Error = SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_ready_unpin(cx)
            .map_err(|_| SendError::Disconnected)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        self.0
            .start_send_unpin(item)
            .map_err(|_| SendError::Disconnected)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_flush_unpin(cx)
            .map_err(|_| SendError::Disconnected)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.0
            .poll_close_unpin(cx)
            .map_err(|_| SendError::Disconnected)
    }
}

#[derive(Debug)]
pub enum SendError {
    Disconnected,
}

impl Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for SendError {}

pub type RecvError = NoError;

#[derive(Debug)]
pub enum OpenBiError {
    Disconnected,
}

impl Display for OpenBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for OpenBiError {}

#[derive(Debug)]
pub struct MemChannelTypes;

impl ChannelTypes for MemChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type Channel<In: RpcMessage, Out: RpcMessage> = self::Channel<In, Out>;
}

impl<In: RpcMessage, Out: RpcMessage> crate::Channel<In, Out, MemChannelTypes>
    for Channel<In, Out>
{
    fn open_bi(&mut self) -> OpenBiFuture<'_, In, Out> {
        let (local_send, remote_recv) = flume::bounded::<Out>(128);
        let (remote_send, local_recv) = flume::bounded::<In>(128);
        let remote_recv = RecvStream(remote_recv.into_stream());
        let local_recv = RecvStream(local_recv.into_stream());
        let remote_send = SendSink(remote_send.into_sink());
        let local_send = SendSink(local_send.into_sink());
        let inner = self.sink.send_async((remote_send, remote_recv));
        OpenBiFuture::new(inner, (local_send, local_recv))
    }

    fn accept_bi(&mut self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(self.stream.recv_async())
    }
}

pub fn connection<Req: RpcMessage, Res: RpcMessage>(
    buffer: usize,
) -> (Channel<Req, Res>, Channel<Res, Req>) {
    let (send1, recv1) = flume::bounded::<Socket<Req, Res>>(buffer);
    let (send2, recv2) = flume::bounded::<Socket<Res, Req>>(buffer);
    (
        Channel {
            stream: recv1,
            sink: send2,
        },
        Channel {
            stream: recv2,
            sink: send1,
        },
    )
}
