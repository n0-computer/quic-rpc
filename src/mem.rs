use core::fmt;
use futures::{channel::mpsc, Future, FutureExt, SinkExt, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::{pin::Pin, result, task::Poll};

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
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.0.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(item)) => std::task::Poll::Ready(Some(Ok(item))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

type Socket<Req, Res> = (self::SendSink<Req>, self::RecvStream<Res>);

pub struct Channel<Req, Res> {
    stream: mpsc::Receiver<Socket<Req, Res>>,
    sink: mpsc::Sender<Socket<Res, Req>>,
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
pub struct OpenBiFuture<'a, Req, Res> {
    #[pin]
    inner: futures::sink::Send<'a, mpsc::Sender<Socket<Res, Req>>, Socket<Res, Req>>,
    res: Option<Socket<Req, Res>>,
}

impl<'a, Req, Res> OpenBiFuture<'a, Req, Res> {
    fn new(
        inner: futures::sink::Send<'a, mpsc::Sender<Socket<Res, Req>>, Socket<Res, Req>>,
        res: Socket<Req, Res>,
    ) -> Self {
        Self {
            inner,
            res: Some(res),
        }
    }
}

impl<'a, Req, Res> Future for OpenBiFuture<'a, Req, Res> {
    type Output = result::Result<Socket<Req, Res>, mpsc::SendError>;

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
            Poll::Ready(Err(cause)) => Poll::Ready(Err(cause)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[pin_project]
pub struct AcceptBiFuture<'a, Req, Res>(
    #[pin] futures::stream::Next<'a, mpsc::Receiver<Socket<Req, Res>>>,
);

impl<'a, Req, Res> Future for AcceptBiFuture<'a, Req, Res> {
    type Output = result::Result<Socket<Req, Res>, AcceptBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.project().0.poll_unpin(cx) {
            Poll::Ready(Some(socket)) => Poll::Ready(Ok(socket)),
            Poll::Ready(None) => Poll::Ready(Err(AcceptBiError::SenderDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub type SendSink<Req> = mpsc::Sender<Req>;

pub type SendError = mpsc::SendError;

pub type RecvError = NoError;

pub type OpenBiError = mpsc::SendError;

impl<
        Req: Serialize + DeserializeOwned + Send + Unpin + 'static,
        Res: Serialize + DeserializeOwned + Send + 'static,
    > crate::Channel<Req, Res> for Channel<Req, Res>
{
    type SendSink<M: Serialize + Unpin> = self::SendSink<M>;

    type RecvStream<M: DeserializeOwned> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a> = self::OpenBiFuture<'a, Req, Res>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let (local_send, remote_recv) = mpsc::channel::<Req>(1);
        let (remote_send, local_recv) = mpsc::channel::<Res>(1);
        let remote_recv = RecvStream(remote_recv);
        let local_recv = RecvStream(local_recv);
        let inner = self.sink.send((remote_send, remote_recv));
        OpenBiFuture::new(inner, (local_send, local_recv))
    }

    type AcceptBiError = AcceptBiError;

    type AcceptBiFuture<'a> = self::AcceptBiFuture<'a, Req, Res>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        AcceptBiFuture(self.stream.next())
    }
}

pub fn connection<Req, Res>(buffer: usize) -> (Channel<Req, Res>, Channel<Res, Req>) {
    let (send1, recv1) = mpsc::channel::<Socket<Req, Res>>(buffer);
    let (send2, recv2) = mpsc::channel::<Socket<Res, Req>>(buffer);
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
