//! Channel that combines two other channels
use crate::{ChannelTypes, RpcMessage};
use futures::{
    future::{self, BoxFuture},
    FutureExt, Sink, Stream, TryFutureExt,
};
use pin_project::pin_project;
use std::{
    error, fmt,
    fmt::Debug,
    marker::PhantomData,
    pin::Pin,
    result,
    task::{Context, Poll},
};

/// A channel that combines two other channels
pub struct ClientChannel<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> {
    a: Option<A::ClientChannel<In, Out>>,
    b: Option<B::ClientChannel<In, Out>>,
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage>
    ClientChannel<A, B, In, Out>
{
    /// Create a combined channel from two other channels
    ///
    /// When opening a channel with [`crate::Channel::open_bi`], the first configured channel will be used,
    /// and no attempt will be made to use the second channel in case of failure. If no channels are
    /// configred, open_bi will immediately fail with [`OpenBiError::NoChannel`].
    ///
    /// When listening for incoming channels with [`crate::Channel::accept_bi`], all configured channels will
    /// be listened on, and the first to receive a connection will be used. If no channels are
    /// configured, accept_bi will wait forever.
    pub fn new(a: Option<A::ClientChannel<In, Out>>, b: Option<B::ClientChannel<In, Out>>) -> Self {
        Self { a, b }
    }
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> Clone
    for ClientChannel<A, B, In, Out>
{
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
        }
    }
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> Debug
    for ClientChannel<A, B, In, Out>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("a", &self.a)
            .field("b", &self.b)
            .finish()
    }
}

/// A channel that combines two other channels
pub struct ServerChannel<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> {
    a: Option<A::ServerChannel<In, Out>>,
    b: Option<B::ServerChannel<In, Out>>,
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage>
    ServerChannel<A, B, In, Out>
{
    /// Create a combined channel from two other channels
    ///
    /// When opening a channel with [`crate::Channel::open_bi`], the first configured channel will be used,
    /// and no attempt will be made to use the second channel in case of failure. If no channels are
    /// configred, open_bi will immediately fail with [`OpenBiError::NoChannel`].
    ///
    /// When listening for incoming channels with [`crate::Channel::accept_bi`], all configured channels will
    /// be listened on, and the first to receive a connection will be used. If no channels are
    /// configured, accept_bi will wait forever.
    pub fn new(a: Option<A::ServerChannel<In, Out>>, b: Option<B::ServerChannel<In, Out>>) -> Self {
        Self { a, b }
    }
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> Clone
    for ServerChannel<A, B, In, Out>
{
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
        }
    }
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage> Debug
    for ServerChannel<A, B, In, Out>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("a", &self.a)
            .field("b", &self.b)
            .finish()
    }
}

/// SendSink for combined channels
#[pin_project(project = SendSinkProj)]
pub enum SendSink<A: ChannelTypes, B: ChannelTypes, Out: RpcMessage> {
    /// A variant
    A(#[pin] A::SendSink<Out>),
    /// B variant
    B(#[pin] B::SendSink<Out>),
}

impl<A: ChannelTypes, B: ChannelTypes, Out: RpcMessage> Sink<Out> for SendSink<A, B, Out> {
    type Error = self::SendError<A, B>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_ready(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_ready(cx).map_err(Self::Error::B),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        match self.project() {
            SendSinkProj::A(sink) => sink.start_send(item).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.start_send(item).map_err(Self::Error::B),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_flush(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_flush(cx).map_err(Self::Error::B),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_close(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_close(cx).map_err(Self::Error::B),
        }
    }
}

/// RecvStream for combined channels
#[pin_project(project = ResStreamProj)]
pub enum RecvStream<A: ChannelTypes, B: ChannelTypes, In: RpcMessage> {
    /// A variant
    A(#[pin] A::RecvStream<In>),
    /// B variant
    B(#[pin] B::RecvStream<In>),
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage> Stream for RecvStream<A, B, In> {
    type Item = Result<In, RecvError<A, B>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            ResStreamProj::A(stream) => stream.poll_next(cx).map_err(RecvError::<A, B>::A),
            ResStreamProj::B(stream) => stream.poll_next(cx).map_err(RecvError::<A, B>::B),
        }
    }
}

/// SendError for combined channels
#[derive(Debug)]
pub enum SendError<A: ChannelTypes, B: ChannelTypes> {
    /// A variant
    A(A::SendError),
    /// B variant
    B(B::SendError),
}

impl<A: ChannelTypes, B: ChannelTypes> fmt::Display for SendError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ChannelTypes, B: ChannelTypes> error::Error for SendError<A, B> {}

/// RecvError for combined channels
#[derive(Debug)]
pub enum RecvError<A: ChannelTypes, B: ChannelTypes> {
    /// A variant
    A(A::RecvError),
    /// B variant
    B(B::RecvError),
}

impl<A: ChannelTypes, B: ChannelTypes> fmt::Display for RecvError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ChannelTypes, B: ChannelTypes> error::Error for RecvError<A, B> {}

/// OpenBiError for combined channels
#[derive(Debug)]
pub enum OpenBiError<A: ChannelTypes, B: ChannelTypes> {
    /// A variant
    A(A::OpenBiError),
    /// B variant
    B(B::OpenBiError),
    /// None of the two channels is configured
    NoChannel,
}

impl<A: ChannelTypes, B: ChannelTypes> fmt::Display for OpenBiError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ChannelTypes, B: ChannelTypes> error::Error for OpenBiError<A, B> {}

/// AcceptBiError for combined channels
#[derive(Debug)]
pub enum AcceptBiError<A: ChannelTypes, B: ChannelTypes> {
    /// A variant
    A(A::AcceptBiError),
    /// B variant
    B(B::AcceptBiError),
}

impl<A: ChannelTypes, B: ChannelTypes> fmt::Display for AcceptBiError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ChannelTypes, B: ChannelTypes> error::Error for AcceptBiError<A, B> {}

/// AcceptBiError for combined channels
#[derive(Debug, Clone)]
pub enum CreateChannelError<A: ChannelTypes, B: ChannelTypes> {
    /// A variant
    A(A::CreateChannelError),
    /// B variant
    B(B::CreateChannelError),
}

impl<A: ChannelTypes, B: ChannelTypes> fmt::Display for CreateChannelError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ChannelTypes, B: ChannelTypes> error::Error for CreateChannelError<A, B> {}

/// Future returned by open_bi
pub type OpenBiFuture<'a, A, B, In, Out> =
    BoxFuture<'a, result::Result<Socket<A, B, In, Out>, self::OpenBiError<A, B>>>;

/// Future returned by accept_bi
pub type AcceptBiFuture<'a, A, B, In, Out> =
    BoxFuture<'a, result::Result<self::Socket<A, B, In, Out>, self::AcceptBiError<A, B>>>;

type Socket<A, B, In, Out> = (self::SendSink<A, B, Out>, self::RecvStream<A, B, In>);

/// Channel types for combined channels
///
/// `A` and `B` are the channel types for the two channels.
/// `In` and `Out` are the message types for the two channels.
#[derive(Debug, Clone, Copy)]
pub struct CombinedChannelTypes<A: ChannelTypes, B: ChannelTypes>(PhantomData<(A, B)>);

impl<A: ChannelTypes, B: ChannelTypes> crate::ChannelTypes for CombinedChannelTypes<A, B> {
    type CreateChannelError = self::CreateChannelError<A, B>;

    type SendSink<M: RpcMessage> = self::SendSink<A, B, M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<A, B, M>;

    type SendError = self::SendError<A, B>;

    type RecvError = self::RecvError<A, B>;

    type OpenBiError = self::OpenBiError<A, B>;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, A, B, In, Out>;

    type AcceptBiError = self::AcceptBiError<A, B>;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> =
        self::AcceptBiFuture<'a, A, B, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::ClientChannel<A, B, In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::ServerChannel<A, B, In, Out>;
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage>
    crate::ClientChannel<In, Out, CombinedChannelTypes<A, B>> for ClientChannel<A, B, In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, A, B, In, Out> {
        async {
            // try a first, then b
            if let Some(a) = &self.a {
                let (send, recv) = a.open_bi().await.map_err(OpenBiError::A)?;
                Ok((SendSink::A(send), RecvStream::A(recv)))
            } else if let Some(b) = &self.b {
                let (send, recv) = b.open_bi().await.map_err(OpenBiError::B)?;
                Ok((SendSink::B(send), RecvStream::B(recv)))
            } else {
                future::err(OpenBiError::NoChannel).await
            }
        }
        .boxed()
    }
}

impl<A: ChannelTypes, B: ChannelTypes, In: RpcMessage, Out: RpcMessage>
    crate::ServerChannel<In, Out, CombinedChannelTypes<A, B>> for ServerChannel<A, B, In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, A, B, In, Out> {
        let a_fut = if let Some(a) = &self.a {
            a.accept_bi()
                .map_ok(|(send, recv)| {
                    (
                        SendSink::<A, B, Out>::A(send),
                        RecvStream::<A, B, In>::A(recv),
                    )
                })
                .map_err(AcceptBiError::A)
                .left_future()
        } else {
            future::pending().right_future()
        };
        let b_fut = if let Some(b) = &self.b {
            b.accept_bi()
                .map_ok(|(send, recv)| {
                    (
                        SendSink::<A, B, Out>::B(send),
                        RecvStream::<A, B, In>::B(recv),
                    )
                })
                .map_err(AcceptBiError::B)
                .left_future()
        } else {
            future::pending().right_future()
        };
        async move {
            tokio::select! {
                res = a_fut => res,
                res = b_fut => res,
            }
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{transport::{combined, mem}, ClientChannel};

    #[tokio::test]
    async fn open_empty_channel() {
        let channel =
            combined::ClientChannel::<mem::MemChannelTypes, mem::MemChannelTypes, (), ()>::new(
                None, None,
            );
        let res = channel.open_bi().await;
        assert!(matches!(res, Err(OpenBiError::NoChannel)));
    }
}
