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

pub enum Channel<In, Out> {
    Mem(mem::Channel<In, Out>),
    Quinn(::quinn::Connection),
}

#[pin_project(project = SendSinkProj)]
pub enum SendSink<Out> {
    Mem(#[pin] mem::SendSink<Out>),
    Quinn(#[pin] quinn::SendSink<Out>),
}

#[pin_project(project = ResStreamProj)]
pub enum RecvStream<In> {
    Mem(#[pin] mem::RecvStream<In>),
    Quinn(#[pin] quinn::RecvStream<In>),
}

impl<Out: Serialize> Sink<Out> for SendSink<Out> {
    type Error = self::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::Mem(sink) => sink.poll_ready(cx).map_err(Error::Mem),
            SendSinkProj::Quinn(sink) => sink.poll_ready(cx).map_err(Error::Quinn),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
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

impl<In: DeserializeOwned> Stream for RecvStream<In> {
    type Item = Result<In, io::Error>;

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

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

impl<
        In: Serialize + DeserializeOwned + Send + 'static,
        Out: Serialize + DeserializeOwned + Send + Unpin + 'static,
    > crate::Channel<In, Out> for Channel<In, Out>
{
    type SendSink<M: Serialize + Unpin> = self::SendSink<M>;

    type RecvStream<M: DeserializeOwned> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a> = BoxFuture<'a, result::Result<Socket<In, Out>, Self::OpenBiError>>;

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
        BoxFuture<'a, result::Result<self::Socket<In, Out>, Self::AcceptBiError>>;

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
