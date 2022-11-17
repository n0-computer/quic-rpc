use crate::mem;
use crate::quinn;
use crate::ChannelTypes;
use crate::RpcMessage;
use futures::{future::BoxFuture, FutureExt, Sink, Stream};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::fmt;
use std::fmt::Debug;
use std::{
    io,
    pin::Pin,
    result,
    task::{Context, Poll},
};

pub struct Channel<In, Out> {
    mem: mem::Channel<In, Out>,
    quinn: ::quinn::Connection,
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

impl<M: Debug, Q: Debug> fmt::Display for Error<M, Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

pub type SendError = Error<mem::SendError, io::Error>;

pub type RecvError = io::Error;

pub type OpenBiError = Error<mem::OpenBiError, quinn::OpenBiError>;

pub type AcceptBiError = Error<mem::AcceptBiError, quinn::AcceptBiError>;

pub type OpenBiFuture<'a, In, Out> =
    BoxFuture<'a, result::Result<Socket<In, Out>, self::OpenBiError>>;

pub type AcceptBiFuture<'a, In, Out> =
    BoxFuture<'a, result::Result<self::Socket<In, Out>, self::AcceptBiError>>;

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

pub struct MemAndQuinnChannelTypes;

impl ChannelTypes for MemAndQuinnChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type Channel<In: RpcMessage, Out: RpcMessage> = self::Channel<In, Out>;
}

impl<In: RpcMessage, Out: RpcMessage> crate::Channel<In, Out, MemAndQuinnChannelTypes>
    for Channel<In, Out>
{
    fn open_bi(&mut self) -> OpenBiFuture<'_, In, Out> {
        // since we got both, prefer mem
        async {
            let (send, recv) = self.mem.open_bi().await.map_err(Error::Mem)?;
            Ok((SendSink::Mem(send), RecvStream::Mem(recv)))
        }
        .boxed()
    }

    fn accept_bi(&mut self) -> AcceptBiFuture<'_, In, Out> {
        let mem_future = self.mem.accept_bi();
        // disambiguate accept_bi call so we don't get the one directly from quinn::Connection
        let quinn_future =
            <::quinn::Connection as crate::Channel<In, Out, _>>::accept_bi(&mut self.quinn);
        async move {
            tokio::select! {
                res = mem_future => res.map(|(send, recv)| (SendSink::Mem(send), RecvStream::Mem(recv))).map_err(Error::Mem),
                res = quinn_future => res.map(|(send, recv)| (SendSink::Quinn(send), RecvStream::Quinn(recv))).map_err(Error::Quinn),
            }
        }.boxed()
    }
}
