use crate::mem;
use crate::quinn;
use either::Either;
use futures::future::BoxFuture;
use futures::FutureExt;
use futures::Sink;
use futures::Stream;
use pin_project::pin_project;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::io;
use std::pin::Pin;
use std::result;
use std::task::Context;
use std::task::Poll;

pub type Channel<Req, Res> = Either<mem::Channel<Req, Res>, ::quinn::Connection>;

#[pin_project(project = ReqSinkProj)]
pub enum ReqSink<Req> {
    Mem(#[pin] mem::ReqSink<Req>),
    Quinn(#[pin] quinn::ReqSink<Req>),
}

#[pin_project(project = ResStreamProj)]
pub enum ResStream<Res> {
    Mem(#[pin] mem::ResStream<Res>),
    Quinn(#[pin] quinn::ResStream<Res>),
}

impl<Req: Serialize> Sink<Req> for ReqSink<Req> {
    type Error = self::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            ReqSinkProj::Mem(sink) => sink.poll_ready(cx).map_err(Either::Left),
            ReqSinkProj::Quinn(sink) => sink.poll_ready(cx).map_err(Either::Right),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Req) -> Result<(), Self::Error> {
        match self.project() {
            ReqSinkProj::Mem(sink) => sink.start_send(item).map_err(Either::Left),
            ReqSinkProj::Quinn(sink) => sink.start_send(item).map_err(Either::Right),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            ReqSinkProj::Mem(sink) => sink.poll_flush(cx).map_err(Either::Left),
            ReqSinkProj::Quinn(sink) => sink.poll_flush(cx).map_err(Either::Right),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            ReqSinkProj::Mem(sink) => sink.poll_close(cx).map_err(Either::Left),
            ReqSinkProj::Quinn(sink) => sink.poll_close(cx).map_err(Either::Right),
        }
    }
}

impl<Res: DeserializeOwned> Stream for ResStream<Res> {
    type Item = Result<Res, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            ResStreamProj::Mem(stream) => match stream.poll_next(cx) {
                Poll::Ready(Some(x)) => Poll::Ready(Some(Ok(x.unwrap()))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
            ResStreamProj::Quinn(stream) => stream.poll_next(cx),
        }
    }
}

pub type SendError = Either<mem::SendError, io::Error>;

pub type RecvError = io::Error;

pub type OpenBiError = Either<mem::OpenBiError, quinn::OpenBiError>;

pub type AcceptBiError = Either<mem::AcceptBiError, quinn::AcceptBiError>;

type Socket<Req, Res> = (self::ReqSink<Req>, self::ResStream<Res>);

impl<Req: Serialize + Send + 'static, Res: DeserializeOwned + Send + 'static>
    crate::Channel<Req, Res> for Channel<Req, Res>
{
    type ReqSink = self::ReqSink<Req>;

    type ResStream = self::ResStream<Res>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a> = BoxFuture<'a, result::Result<Socket<Req, Res>, Self::OpenBiError>>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        match self {
            Either::Left(mem) => async {
                let (send, recv) = mem.open_bi().await.map_err(Either::Left)?;
                Ok((ReqSink::Mem(send), ResStream::Mem(recv)))
            }
            .boxed(),
            Either::Right(quinn) => async {
                let (send, recv) = quinn.open_bi().await.map_err(Either::Right)?;
                Ok((ReqSink::Quinn(send), ResStream::Quinn(recv)))
            }
            .boxed(),
        }
    }

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a> =
        BoxFuture<'a, result::Result<self::Socket<Req, Res>, Self::AcceptBiError>>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        match self {
            Either::Left(mem) => async {
                let (send, recv) = mem.accept_bi().await.map_err(Either::Left)?;
                Ok((ReqSink::Mem(send), ResStream::Mem(recv)))
            }
            .boxed(),
            Either::Right(quinn) => async {
                let (send, recv) = quinn.accept_bi().await.map_err(Either::Right)?;
                Ok((ReqSink::Quinn(send), ResStream::Quinn(recv)))
            }
            .boxed(),
        }
    }
}
