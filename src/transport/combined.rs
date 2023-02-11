//! Channel that combines two other channels
use crate::{Connection, ConnectionErrors, RpcMessage};
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
pub struct CombinedClientChannel<A, B, In: RpcMessage, Out: RpcMessage> {
    a: Option<A>,
    b: Option<B>,
    _p: PhantomData<(In, Out)>,
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage>
    CombinedClientChannel<A, B, In, Out>
{
    /// Create a combined channel from two other channels
    ///
    /// When opening a channel with [`crate::ClientChannel::open_bi`], the first configured channel will be used,
    /// and no attempt will be made to use the second channel in case of failure. If no channels are
    /// configred, open_bi will immediately fail with [`OpenBiError::NoChannel`].
    ///
    /// When listening for incoming channels with [`crate::ServerChannel::accept_bi`], all configured channels will
    /// be listened on, and the first to receive a connection will be used. If no channels are
    /// configured, accept_bi will wait forever.
    pub fn new(a: Option<A>, b: Option<B>) -> Self {
        Self {
            a,
            b,
            _p: PhantomData,
        }
    }
}
impl<A: Clone, B: Clone, In: RpcMessage, Out: RpcMessage> Clone
    for CombinedClientChannel<A, B, In, Out>
{
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
            _p: PhantomData,
        }
    }
}

impl<A: Debug, B: Debug, In: RpcMessage, Out: RpcMessage> Debug
    for CombinedClientChannel<A, B, In, Out>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("a", &self.a)
            .field("b", &self.b)
            .finish()
    }
}

/// A channel that combines two other channels
pub struct CombinedServerChannel<A, B, In: RpcMessage, Out: RpcMessage> {
    a: Option<A>,
    b: Option<B>,
    _p: PhantomData<(In, Out)>,
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage>
    CombinedServerChannel<A, B, In, Out>
{
    /// Create a combined channel from two other channels
    ///
    /// When opening a channel with [`crate::ClientChannel::open_bi`], the first configured channel will be used,
    /// and no attempt will be made to use the second channel in case of failure. If no channels are
    /// configred, open_bi will immediately fail with [`OpenBiError::NoChannel`].
    ///
    /// When listening for incoming channels with [`crate::ServerChannel::accept_bi`], all configured channels will
    /// be listened on, and the first to receive a connection will be used. If no channels are
    /// configured, accept_bi will wait forever.
    pub fn new(a: Option<A>, b: Option<B>) -> Self {
        Self {
            a,
            b,
            _p: PhantomData,
        }
    }
}

impl<A: Clone, B: Clone, In: RpcMessage, Out: RpcMessage> Clone
    for CombinedServerChannel<A, B, In, Out>
{
    fn clone(&self) -> Self {
        Self {
            a: self.a.clone(),
            b: self.b.clone(),
            _p: PhantomData,
        }
    }
}

impl<A: Debug, B: Debug, In: RpcMessage, Out: RpcMessage> Debug
    for CombinedServerChannel<A, B, In, Out>
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
pub enum SendSink<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage> {
    /// A variant
    A(#[pin] A::SendSink),
    /// B variant
    B(#[pin] B::SendSink),
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage> Sink<Out>
    for SendSink<A, B, In, Out>
{
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
pub enum RecvStream<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage>
{
    /// A variant
    A(#[pin] A::RecvStream),
    /// B variant
    B(#[pin] B::RecvStream),
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage> Stream
    for RecvStream<A, B, In, Out>
{
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
pub enum SendError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::SendError),
    /// B variant
    B(B::SendError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for SendError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for SendError<A, B> {}

/// RecvError for combined channels
#[derive(Debug)]
pub enum RecvError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::RecvError),
    /// B variant
    B(B::RecvError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for RecvError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for RecvError<A, B> {}

/// OpenBiError for combined channels
#[derive(Debug)]
pub enum OpenBiError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::OpenError),
    /// B variant
    B(B::OpenError),
    /// None of the two channels is configured
    NoChannel,
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for OpenBiError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for OpenBiError<A, B> {}

/// AcceptBiError for combined channels
#[derive(Debug)]
pub enum AcceptBiError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::OpenError),
    /// B variant
    B(B::OpenError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for AcceptBiError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for AcceptBiError<A, B> {}

/// Future returned by open_bi
pub type OpenBiFuture<'a, A, B, In, Out> =
    BoxFuture<'a, result::Result<Socket<A, B, In, Out>, self::OpenBiError<A, B>>>;

/// Future returned by accept_bi
pub type AcceptBiFuture<'a, A, B, In, Out> =
    BoxFuture<'a, result::Result<self::Socket<A, B, In, Out>, self::AcceptBiError<A, B>>>;

type Socket<A, B, In, Out> = (
    self::SendSink<A, B, In, Out>,
    self::RecvStream<A, B, In, Out>,
);

impl<A: ConnectionErrors, B: ConnectionErrors, In: RpcMessage, Out: RpcMessage>
    crate::ConnectionErrors for CombinedClientChannel<A, B, In, Out>
{
    type SendError = self::SendError<A, B>;
    type RecvError = self::RecvError<A, B>;
    type OpenError = self::OpenBiError<A, B>;
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage>
    crate::Connection<In, Out> for CombinedClientChannel<A, B, In, Out>
{
    fn next(&self) -> OpenBiFuture<'_, A, B, In, Out> {
        async {
            // try a first, then b
            if let Some(a) = &self.a {
                let (send, recv) = a.next().await.map_err(OpenBiError::A)?;
                Ok((SendSink::A(send), RecvStream::A(recv)))
            } else if let Some(b) = &self.b {
                let (send, recv) = b.next().await.map_err(OpenBiError::B)?;
                Ok((SendSink::B(send), RecvStream::B(recv)))
            } else {
                future::err(OpenBiError::NoChannel).await
            }
        }
        .boxed()
    }

    type RecvStream = self::RecvStream<A, B, In, Out>;

    type SendSink = self::SendSink<A, B, In, Out>;

    type NextFut<'a> = OpenBiFuture<'a, A, B, In, Out>;
}

impl<A: ConnectionErrors, B: ConnectionErrors, In: RpcMessage, Out: RpcMessage>
    crate::ConnectionErrors for CombinedServerChannel<A, B, In, Out>
{
    type SendError = self::SendError<A, B>;
    type RecvError = self::RecvError<A, B>;
    type OpenError = self::AcceptBiError<A, B>;
}

impl<A: Connection<In, Out>, B: Connection<In, Out>, In: RpcMessage, Out: RpcMessage>
    crate::Connection<In, Out> for CombinedServerChannel<A, B, In, Out>
{
    fn next(&self) -> AcceptBiFuture<'_, A, B, In, Out> {
        let a_fut = if let Some(a) = &self.a {
            a.next()
                .map_ok(|(send, recv)| {
                    (
                        SendSink::<A, B, In, Out>::A(send),
                        RecvStream::<A, B, In, Out>::A(recv),
                    )
                })
                .map_err(AcceptBiError::A)
                .left_future()
        } else {
            future::pending().right_future()
        };
        let b_fut = if let Some(b) = &self.b {
            b.next()
                .map_ok(|(send, recv)| {
                    (
                        SendSink::<A, B, In, Out>::B(send),
                        RecvStream::<A, B, In, Out>::B(recv),
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

    type RecvStream = self::RecvStream<A, B, In, Out>;

    type SendSink = self::SendSink<A, B, In, Out>;

    type NextFut<'a> = AcceptBiFuture<'a, A, B, In, Out>;
}

#[cfg(test)]
mod tests {
    
    

    #[tokio::test]
    async fn open_empty_channel() {
        // let channel = combined::CombinedClientChannel::<
        //     mem::MemChannelTypes,
        //     mem::MemChannelTypes,
        //     (),
        //     (),
        // >::new(None, None);
        // let res = channel.open_bi().await;
        // assert!(matches!(res, Err(OpenBiError::NoChannel)));
    }
}
