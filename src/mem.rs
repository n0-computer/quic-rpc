use crate::{ChannelTypes, RpcMessage};
use core::fmt;
use futures::{channel::mpsc, Future, FutureExt, StreamExt};
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

#[pin_project]
pub struct RecvStream<Res>(#[pin] mpsc::Receiver<Res>);

impl<Res> futures::Stream for RecvStream<Res> {
    type Item = Result<Res, NoError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.0.poll_next_unpin(cx) {
            Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

pub struct Channel<In, Out> {
    stream: flume::Receiver<Socket<In, Out>>,
    sink: flume::Sender<Socket<Out, In>>,
}

impl<In, Out> Clone for Channel<In, Out> {
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
pub struct OpenBiFuture<'a, In, Out> {
    #[pin]
    inner: flume::r#async::SendFut<'a, Socket<Out, In>>,
    res: Option<Socket<In, Out>>,
}

impl<'a, In, Out> OpenBiFuture<'a, In, Out> {
    fn new(inner: flume::r#async::SendFut<'a, Socket<Out, In>>, res: Socket<In, Out>) -> Self {
        Self {
            inner,
            res: Some(res),
        }
    }
}

impl<'a, In, Out> Future for OpenBiFuture<'a, In, Out> {
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

#[pin_project]
pub struct AcceptBiFuture<'a, In, Out>(#[pin] flume::r#async::RecvFut<'a, Socket<In, Out>>);

impl<'a, In, Out> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<Socket<In, Out>, AcceptBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.project().0.poll_unpin(cx) {
            Poll::Ready(Ok(socket)) => Poll::Ready(Ok(socket)),
            Poll::Ready(Err(_)) => Poll::Ready(Err(AcceptBiError::SenderDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub type SendSink<Out> = mpsc::Sender<Out>;

pub type SendError = mpsc::SendError;

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
        let (local_send, remote_recv) = mpsc::channel::<Out>(128);
        let (remote_send, local_recv) = mpsc::channel::<In>(128);
        let remote_recv = RecvStream(remote_recv);
        let local_recv = RecvStream(local_recv);
        let inner = self.sink.send_async((remote_send, remote_recv));
        OpenBiFuture::new(inner, (local_send, local_recv))
    }

    fn accept_bi(&mut self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(self.stream.recv_async())
    }
}

pub fn connection<Req, Res>(buffer: usize) -> (Channel<Req, Res>, Channel<Res, Req>) {
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
