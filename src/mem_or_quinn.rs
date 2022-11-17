use crate::mem;
use crate::quinn;
use futures::{future::BoxFuture, FutureExt, Sink, Stream};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    io,
    pin::Pin,
    result,
    task::{Context, Poll},
};

pub enum Channel<Req, Res> {
    Mem(mem::Channel<Req, Res>),
    Quinn(::quinn::Connection),
}

#[pin_project(project = SendSinkProj)]
pub enum SendSink<Req> {
    Mem(#[pin] mem::SendSink<Req>),
    Quinn(#[pin] quinn::SendSink<Req>),
}

#[pin_project(project = ResStreamProj)]
pub enum RecvStream<Res> {
    Mem(#[pin] mem::RecvStream<Res>),
    Quinn(#[pin] quinn::RecvStream<Res>),
}

impl<Req: Serialize> Sink<Req> for SendSink<Req> {
    type Error = self::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::Mem(sink) => sink.poll_ready(cx).map_err(Error::Mem),
            SendSinkProj::Quinn(sink) => sink.poll_ready(cx).map_err(Error::Quinn),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Req) -> Result<(), Self::Error> {
        match self.project() {
            SendSinkProj::Mem(sink) => sink.start_send(item).map_err(Error::Mem),
            SendSinkProj::Quinn(sink) => sink.start_send(item).map_err(Error::Quinn),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::Mem(sink) => sink.poll_flush(cx).map_err(Error::Mem),
            SendSinkProj::Quinn(sink) => sink.poll_flush(cx).map_err(Error::Quinn),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::Mem(sink) => sink.poll_close(cx).map_err(Error::Mem),
            SendSinkProj::Quinn(sink) => sink.poll_close(cx).map_err(Error::Quinn),
        }
    }
}

impl<Res: DeserializeOwned> Stream for RecvStream<Res> {
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

#[derive(Debug)]
pub enum Error<M, Q> {
    Mem(M),
    Quinn(Q),
}

pub type SendError = Error<mem::SendError, io::Error>;

pub type RecvError = io::Error;

pub type OpenBiError = Error<mem::OpenBiError, quinn::OpenBiError>;

pub type AcceptBiError = Error<mem::AcceptBiError, quinn::AcceptBiError>;

type Socket<Req, Res> = (self::SendSink<Req>, self::RecvStream<Res>);

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

    type OpenBiFuture<'a> = BoxFuture<'a, result::Result<Socket<Req, Res>, Self::OpenBiError>>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        match self {
            Channel::Mem(mem) => async {
                let (send, recv) = mem.open_bi().await.map_err(Error::Mem)?;
                Ok((SendSink::Mem(send), RecvStream::Mem(recv)))
            }
            .boxed(),
            Channel::Quinn(quinn) => async {
                let (send, recv) = quinn.open_bi().await.map_err(Error::Quinn)?;
                Ok((SendSink::Quinn(send), RecvStream::Quinn(recv)))
            }
            .boxed(),
        }
    }

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a> =
        BoxFuture<'a, result::Result<self::Socket<Req, Res>, Self::AcceptBiError>>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        match self {
            Channel::Mem(mem) => async {
                let (send, recv) = mem.accept_bi().await.map_err(Error::Mem)?;
                Ok((SendSink::Mem(send), RecvStream::Mem(recv)))
            }
            .boxed(),
            Channel::Quinn(quinn) => async {
                let (send, recv) = quinn.accept_bi().await.map_err(Error::Quinn)?;
                Ok((SendSink::Quinn(send), RecvStream::Quinn(recv)))
            }
            .boxed(),
        }
    }
}
