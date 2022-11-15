use futures::{channel::mpsc, Future, FutureExt, SinkExt, StreamExt};
use pin_project::pin_project;
use std::{pin::Pin, result, task::Poll};

#[derive(Debug)]
pub enum NoError {}

#[pin_project]
pub struct WrapNoError<S> {
    #[pin]
    inner: S,
}

impl<S: futures::Stream + Unpin> futures::Stream for WrapNoError<S> {
    type Item = Result<S::Item, NoError>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(item)) => std::task::Poll::Ready(Some(Ok(item))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

type Socket<Req, Res> = (self::ReqSink<Req>, self::ResStream<Res>);

pub struct Channel<Req, Res> {
    stream: mpsc::Receiver<Socket<Req, Res>>,
    sink: mpsc::Sender<Socket<Res, Req>>,
}

pub enum AcceptBiError {
    SenderDropped,
}

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

pub type ReqSink<Req> = mpsc::Sender<Req>;

pub type ResStream<Res> = WrapNoError<mpsc::Receiver<Res>>;

pub type SendError = mpsc::SendError;

pub type RecvError = NoError;

pub type OpenBiError = mpsc::SendError;

impl<Req: Send + 'static, Res: Send + 'static> crate::Channel<Req, Res> for Channel<Req, Res> {
    type ReqSink = self::ReqSink<Req>;

    type ResStream = self::ResStream<Res>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a> = self::OpenBiFuture<'a, Req, Res>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let (local_send, remote_recv) = mpsc::channel::<Req>(1);
        let (remote_send, local_recv) = mpsc::channel::<Res>(1);
        let remote_recv = WrapNoError { inner: remote_recv };
        let local_recv = WrapNoError { inner: local_recv };
        let inner = self.sink.send((remote_send, remote_recv));
        OpenBiFuture::new(inner, (local_send, local_recv))
    }

    type AcceptBiError = AcceptBiError;

    type AcceptBiFuture<'a> = self::AcceptBiFuture<'a, Req, Res>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        AcceptBiFuture(self.stream.next())
    }
}
