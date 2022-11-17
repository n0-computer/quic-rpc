use futures::channel::oneshot;
use futures::future;
use futures::future::BoxFuture;
use futures::future::Either;
use futures::stream::BoxStream;
use futures::task;
use futures::Future;
use futures::FutureExt;
use futures::Sink;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use futures::TryFutureExt;
use pin_project::pin_project;
use std::error;
use std::fmt;
use std::marker;
use std::marker::PhantomData;
use std::pin::Pin;
use std::result;
use std::task::Poll;

use crate::mem as underlying;
use crate::Channel;
use crate::Service;

pub type BoxSink<'a, T, E> = Pin<Box<dyn Sink<T, Error = E> + Send>>;

/// A message for a service
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Msg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
    type Pattern: InteractionPattern;
}

pub trait RpcMsg<S: Service>: Into<S::Req> + TryFrom<S::Req> + Send + 'static {
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

impl<S: Service, T: RpcMsg<S>> Msg<S> for T {
    type Update = Self;

    type Response = T::Response;

    type Pattern = Rpc;
}

pub trait InteractionPattern: 'static {}

pub struct Rpc;
impl InteractionPattern for Rpc {}

pub struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}

pub struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}

pub struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}

pub enum NoRequest {}

pub struct ClientChannel<S: Service> {
    channel: underlying::Channel<S::Res, S::Req>,
    _s: PhantomData<S>,
}

/// Error for rpc interactions
#[derive(Debug)]
pub enum RpcClientError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
    /// Server closed the stream before sending a response
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl fmt::Display for RpcClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for RpcClientError {}

#[derive(Debug)]
pub enum BidiError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

impl fmt::Display for BidiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for BidiError {}

#[derive(Debug)]
pub enum ClientStreamingError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

impl fmt::Display for ClientStreamingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for ClientStreamingError {}

#[derive(Debug)]
pub enum ClientStreamingItemError {
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl fmt::Display for ClientStreamingItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for ClientStreamingItemError {}

#[derive(Debug)]
pub enum StreamingResponseError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

impl fmt::Display for StreamingResponseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for StreamingResponseError {}

#[derive(Debug)]
pub enum StreamingResponseItemError {
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl fmt::Display for StreamingResponseItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for StreamingResponseItemError {}

#[derive(Debug)]
pub enum BidiItemError {
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl fmt::Display for BidiItemError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: Service> ClientChannel<S> {
    pub fn new(channel: underlying::Channel<S::Res, S::Req>) -> Self {
        Self {
            channel,
            _s: PhantomData,
        }
    }

    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&mut self, msg: M) -> result::Result<M::Response, RpcClientError>
    where
        M: Msg<S, Pattern = Rpc> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.channel.open_bi().await.map_err(RpcClientError::Open)?;
        send.send(msg).await.map_err(RpcClientError::Send)?;
        let res = recv
            .next()
            .await
            .ok_or(RpcClientError::EarlyClose)?
            .map_err(RpcClientError::RecvError)?;
        M::Response::try_from(res).map_err(|_| RpcClientError::DowncastError)
    }

    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn server_streaming<M>(
        &mut self,
        msg: M,
    ) -> result::Result<
        BoxStream<'static, result::Result<M::Response, StreamingResponseItemError>>,
        StreamingResponseError,
    >
    where
        M: Msg<S, Pattern = ServerStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, recv) = self
            .channel
            .open_bi()
            .map_err(StreamingResponseError::Open)
            .await?;
        send.send(msg).map_err(StreamingResponseError::Send).await?;
        let recv = recv
            .map(|x| match x {
                Ok(x) => {
                    M::Response::try_from(x).map_err(|_| StreamingResponseItemError::DowncastError)
                }
                Err(e) => Err(StreamingResponseItemError::RecvError(e)),
            })
            .boxed();
        Ok(recv)
    }

    /// Call to the server that allows the client to stream, single response
    pub async fn client_streaming<M>(
        &mut self,
        msg: M,
    ) -> result::Result<
        (
            BoxSink<'static, M::Update, underlying::SendError>,
            BoxFuture<'static, result::Result<M::Response, ClientStreamingItemError>>,
        ),
        ClientStreamingError,
    >
    where
        M: Msg<S, Pattern = ClientStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self
            .channel
            .open_bi()
            .map_err(ClientStreamingError::Open)
            .await?;
        send.send(msg).map_err(ClientStreamingError::Send).await?;
        let send = send.with(|x: M::Update| future::ok::<S::Req, underlying::SendError>(x.into()));
        let send = Box::pin(send);
        let recv = async move {
            let item = recv
                .next()
                .await
                .ok_or(ClientStreamingItemError::EarlyClose)?;

            match item {
                Ok(x) => {
                    M::Response::try_from(x).map_err(|_| ClientStreamingItemError::DowncastError)
                }
                Err(e) => Err(ClientStreamingItemError::RecvError(e)),
            }
        }
        .boxed();
        Ok((send, recv))
    }

    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn bidi<M>(
        &mut self,
        msg: M,
    ) -> result::Result<
        (
            BoxSink<'static, M::Update, underlying::SendError>,
            BoxStream<'static, result::Result<M::Response, BidiItemError>>,
        ),
        BidiError,
    >
    where
        M: Msg<S, Pattern = BidiStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, recv) = self.channel.open_bi().await.map_err(BidiError::Open)?;
        send.send(msg).await.map_err(BidiError::Send)?;
        let send = send.with(|x: M::Update| future::ok::<S::Req, underlying::SendError>(x.into()));
        let send = Box::pin(send);
        let recv = recv
            .map(|x| match x {
                Ok(x) => M::Response::try_from(x).map_err(|_| BidiItemError::DowncastError),
                Err(e) => Err(BidiItemError::RecvError(e)),
            })
            .boxed();
        Ok((send, recv))
    }
}

pub struct DispatchHelper<S, C> {
    _s: std::marker::PhantomData<(S, C)>,
}

impl<S, C> Clone for DispatchHelper<S, C> {
    fn clone(&self) -> Self {
        Self {
            _s: std::marker::PhantomData,
        }
    }
}

impl<S, C> Copy for DispatchHelper<S, C> {}

/// All the things that can go wrong on the server side
pub enum RpcServerError<S: Service, C: crate::Channel<S::Req, S::Res>> {
    /// Unable to open a new channel
    AcceptBiError(C::AcceptBiError),
    /// Recv side for a channel was closed before getting the first message
    EarlyClose,
    /// Got an unexpected first message, e.g. an update message
    UnexpectedStartMessage,
    /// Error receiving a message
    RecvError(C::RecvError),
    /// Error sending a response
    SendError(C::SendError),
    /// Got an unexpected update message, e.g. a request message or a non-matching update message
    UnexpectedUpdateMessage,
}

impl<S: Service, C: crate::Channel<S::Req, S::Res>> fmt::Debug for RpcServerError<S, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AcceptBiError(arg0) => f.debug_tuple("AcceptBiError").field(arg0).finish(),
            Self::EarlyClose => write!(f, "EarlyClose"),
            Self::RecvError(arg0) => f.debug_tuple("RecvError").field(arg0).finish(),
            Self::SendError(arg0) => f.debug_tuple("SendError").field(arg0).finish(),
            Self::UnexpectedStartMessage => f.debug_tuple("UnexpectedStartMessage").finish(),
            Self::UnexpectedUpdateMessage => f.debug_tuple("UnexpectedStartMessage").finish(),
        }
    }
}

impl<S: Service, C: crate::Channel<S::Req, S::Res>> fmt::Display for RpcServerError<S, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl<S: Service, C: crate::Channel<S::Req, S::Res>> error::Error for RpcServerError<S, C> {}

impl<S: Service, C: crate::Channel<S::Req, S::Res>> Default for DispatchHelper<S, C> {
    fn default() -> Self {
        Self {
            _s: marker::PhantomData,
        }
    }
}

impl<S: Service, C: crate::Channel<S::Req, S::Res>> DispatchHelper<S, C> {
    /// Accept one channel from the client, pull out the first request, and return both the first
    /// message and the channel for further processing.
    pub async fn accept_one(
        self,
        channel: &mut C,
    ) -> result::Result<(S::Req, (C::SendSink<S::Res>, C::RecvStream<S::Req>)), RpcServerError<S, C>>
    where
        C::RecvStream<S::Req>: Unpin,
    {
        let mut channel = channel
            .accept_bi()
            .await
            .map_err(RpcServerError::AcceptBiError)?;
        // get the first message from the client. This will tell us what it wants to do.
        let request: S::Req = channel
            .1
            .next()
            .await
            // no msg => early close
            .ok_or(RpcServerError::EarlyClose)?
            // recv error
            .map_err(RpcServerError::RecvError)?;
        Ok((request, channel))
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn rpc<M, F, Fut, T>(
        self,
        req: M,
        c: (C::SendSink<S::Res>, C::RecvStream<S::Req>),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<S, C>>
    where
        M: Msg<S, Pattern = Rpc>,
        F: FnOnce(T, M) -> Fut,
        Fut: Future<Output = M::Response>,
        T: Send + 'static,
    {
        let (send, _recv) = c;
        // get the response
        let res = f(target, req).await;
        // turn into a S::Res so we can send it
        let res: S::Res = res.into();
        // send it and return the error if any
        tokio::pin!(send);
        send.send(res).await.map_err(RpcServerError::SendError)
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn client_streaming<M, F, Fut, T>(
        self,
        req: M,
        c: (C::SendSink<S::Res>, C::RecvStream<S::Req>),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<S, C>>
    where
        M: Msg<S, Pattern = ClientStreaming>,
        F: FnOnce(T, M, UpdateDowncaster<S, C, M>) -> Fut + Send + 'static,
        Fut: Future<Output = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (send, recv) = c;
        let (updates, read_error) = UpdateDowncaster::new(recv);
        race2(read_error.map(Err), async move {
            // get the response
            let res = f(target, req, updates).await;
            // turn into a S::Res so we can send it
            let res: S::Res = res.into();
            // send it and return the error if any
            tokio::pin!(send);
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn bidi_streaming<M, F, Str, T>(
        self,
        req: M,
        c: (C::SendSink<S::Res>, C::RecvStream<S::Req>),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<S, C>>
    where
        M: Msg<S, Pattern = BidiStreaming>,
        F: FnOnce(T, M, UpdateDowncaster<S, C, M>) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (send, recv) = c;
        // downcast the updates
        let (updates, read_error) = UpdateDowncaster::new(recv);
        // get the response
        let responses = f(target, req, updates);
        race2(read_error.map(Err), async move {
            tokio::pin!(responses);
            tokio::pin!(send);
            while let Some(response) = responses.next().await {
                // turn into a S::Res so we can send it
                let response: S::Res = response.into();
                // send it and return the error if any
                send.send(response)
                    .await
                    .map_err(RpcServerError::SendError)?;
            }
            Ok(())
        })
        .await
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn server_streaming<M, F, Str, T>(
        self,
        req: M,
        c: (C::SendSink<S::Res>, C::RecvStream<S::Req>),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<S, C>>
    where
        M: Msg<S, Pattern = ServerStreaming>,
        F: FnOnce(T, M) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (send, recv) = c;
        // get the response
        let responses = f(target, req);
        tokio::pin!(responses);
        tokio::pin!(send);
        while let Some(response) = responses.next().await {
            // turn into a S::Res so we can send it
            let response: S::Res = response.into();
            // send it and return the error if any
            send.send(response)
                .await
                .map_err(RpcServerError::SendError)?;
        }
        Ok(())
    }
}

#[pin_project]
pub struct UpdateDowncaster<S: Service, C: Channel<S::Req, S::Res>, M: Msg<S>>(
    #[pin] C::RecvStream<S::Req>,
    Option<oneshot::Sender<RpcServerError<S, C>>>,
    PhantomData<M>,
);

impl<S: Service, C: Channel<S::Req, S::Res>, M: Msg<S>> UpdateDowncaster<S, C, M> {
    fn new(recv: C::RecvStream<S::Req>) -> (Self, impl Future<Output = RpcServerError<S, C>>) {
        let (error_send, error_recv) = oneshot::channel();
        let error_recv = error_recv.map(|x| x.unwrap());
        (Self(recv, Some(error_send), PhantomData), error_recv)
    }
}

impl<S: Service, C: Channel<S::Req, S::Res>, M: Msg<S>> Stream for UpdateDowncaster<S, C, M> {
    type Item = M::Update;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.0.poll_next_unpin(cx) {
            Poll::Ready(Some(msg)) => match msg {
                Ok(msg) => match M::Update::try_from(msg) {
                    Ok(msg) => Poll::Ready(Some(msg)),
                    Err(_cause) => {
                        // we were unable to downcast, so we need to send an error
                        if let Some(tx) = this.1.take() {
                            let _ = tx.send(RpcServerError::UnexpectedUpdateMessage);
                        }
                        Poll::Pending
                    }
                },
                Err(cause) => {
                    // we got a recv error, so return pending and send the error
                    if let Some(tx) = this.1.take() {
                        let _ = tx.send(RpcServerError::RecvError(cause));
                    }
                    Poll::Pending
                }
            },
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

async fn race2<T, A: Future<Output = T>, B: Future<Output = T>>(f1: A, f2: B) -> T {
    tokio::select! {
        x = f1 => x,
        x = f2 => x,
    }
}
