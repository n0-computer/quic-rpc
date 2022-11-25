//! [RpcClient] and support types
//!
//! This defines the RPC client DSL
use crate::{
    message::{BidiStreaming, ClientStreaming, Msg, Rpc, ServerStreaming},
    Channel, ChannelTypes, OpenChannelError, Service,
};
use futures::{
    future::BoxFuture, stream::BoxStream, FutureExt, Sink, SinkExt, Stream, StreamExt, TryFutureExt,
};
use pin_project::pin_project;
use std::{
    error,
    fmt::{self, Debug},
    marker::PhantomData,
    pin::Pin,
    result,
    sync::Arc,
    task::{Context, Poll},
};

/// A channel factory holds a channel or a cause for there being no channel.
///
/// It is informed when there are issues with the current channel and can then decide to
/// create a new channel and replace the old one.
pub trait ChannelFactory<S: Service, C: ChannelTypes>: fmt::Debug + Send + Sync + 'static {
    /// The current channel or reason why there is no channel.
    ///
    /// This method always returns immediately, even if there is no channel. It might trigger
    /// acquisition of a new channel in the background.
    fn current(&self) -> Result<C::Channel<S::Res, S::Req>, OpenChannelError>;

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn open_bi_error(&self, _channel: &C::Channel<S::Res, S::Req>, _error: &C::OpenBiError) {}

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn accept_bi_error(&self, _channel: &C::Channel<S::Res, S::Req>, _error: &C::AcceptBiError) {}
}

struct StaticChannelFactory<S: Service, C: ChannelTypes> {
    channel: C::Channel<S::Res, S::Req>,
}

impl<S: Service, C: ChannelTypes> Debug for StaticChannelFactory<S, C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticChannelFactory")
            .field("channel", &self.channel)
            .finish()
    }
}

impl<S: Service, C: ChannelTypes> ChannelFactory<S, C> for StaticChannelFactory<S, C> {
    fn current(&self) -> Result<C::Channel<S::Res, S::Req>, OpenChannelError> {
        Ok(self.channel.clone())
    }
}

/// A client for a specific service
///
/// This is a wrapper around a [crate::Channel] that serves as the entry point for the client DSL.
/// `S` is the service type, `C` is the channel type.
#[derive(Debug)]
pub struct RpcClient<S: Service, C: ChannelTypes> {
    channel: Arc<dyn ChannelFactory<S, C>>,
    _s: PhantomData<S>,
}

impl<S: Service, C: ChannelTypes> Clone for RpcClient<S, C> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            _s: self._s,
        }
    }
}

/// Sink that can be used to send updates to the server for the two interaction patterns
/// that support it, [ClientStreaming] and [BidiStreaming].
#[pin_project]
#[derive(Debug)]
pub struct UpdateSink<S: Service, C: ChannelTypes, M: Msg<S>>(
    #[pin] C::SendSink<S::Req>,
    PhantomData<M>,
);

impl<S: Service, C: ChannelTypes, M: Msg<S>> Sink<M::Update> for UpdateSink<S, C, M> {
    type Error = C::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: M::Update) -> Result<(), Self::Error> {
        let req: S::Req = item.into();
        self.project().0.start_send_unpin(req)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

/// Either an error opening a channel, or an error opening a subchannel
#[derive(Debug)]
pub enum ClientOpenBiError<C: ChannelTypes> {
    /// Error opening the channel
    OpenChannelError(crate::OpenChannelError),
    /// Error opening the subchannel
    OpenBiError(C::OpenBiError),
}

impl<S: Service, C: ChannelTypes> RpcClient<S, C> {
    /// Create a new rpc client from a channel
    pub fn new(channel: C::Channel<S::Res, S::Req>) -> Self {
        Self::lazy(Arc::new(StaticChannelFactory { channel }))
    }

    /// Create a new rpc client from a channel holder
    pub fn lazy(channel: Arc<dyn ChannelFactory<S, C>>) -> Self {
        Self {
            channel,
            _s: PhantomData,
        }
    }

    /// Open a bidi connection on an existing channel, or possibly also open a new channel
    async fn open_bi(
        &self,
    ) -> result::Result<(C::SendSink<S::Req>, C::RecvStream<S::Res>), ClientOpenBiError<C>> {
        match self.channel.current() {
            Ok(channel) => match channel.open_bi().await {
                Ok(chan) => Ok(chan),
                Err(e) => {
                    self.channel.open_bi_error(&channel, &e);
                    Err(ClientOpenBiError::OpenBiError(e))
                }
            },
            Err(e) => Err(ClientOpenBiError::OpenChannelError(e)),
        }
    }

    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&self, msg: M) -> result::Result<M::Response, RpcClientError<C>>
    where
        M: Msg<S, Pattern = Rpc> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.open_bi().await.map_err(RpcClientError::Open)?;
        send.send(msg).await.map_err(RpcClientError::Send)?;
        let res = recv
            .next()
            .await
            .ok_or(RpcClientError::EarlyClose)?
            .map_err(RpcClientError::RecvError)?;
        // keep send alive until we have the answer
        drop(send);
        M::Response::try_from(res).map_err(|_| RpcClientError::DowncastError)
    }

    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn server_streaming<M>(
        &self,
        msg: M,
    ) -> result::Result<
        BoxStream<'static, result::Result<M::Response, StreamingResponseItemError<C>>>,
        StreamingResponseError<C>,
    >
    where
        M: Msg<S, Pattern = ServerStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, recv) = self.open_bi().map_err(StreamingResponseError::Open).await?;
        send.send(msg).map_err(StreamingResponseError::Send).await?;
        let recv = recv.map(move |x| match x {
            Ok(x) => {
                M::Response::try_from(x).map_err(|_| StreamingResponseItemError::DowncastError)
            }
            Err(e) => Err(StreamingResponseItemError::RecvError(e)),
        });
        // keep send alive so the request on the server side does not get cancelled
        let recv = DeferDrop(recv, send).boxed();
        Ok(recv)
    }

    /// Call to the server that allows the client to stream, single response
    pub async fn client_streaming<M>(
        &self,
        msg: M,
    ) -> result::Result<
        (
            UpdateSink<S, C, M>,
            BoxFuture<'static, result::Result<M::Response, ClientStreamingItemError<C>>>,
        ),
        ClientStreamingError<C>,
    >
    where
        M: Msg<S, Pattern = ClientStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.open_bi().map_err(ClientStreamingError::Open).await?;
        send.send(msg).map_err(ClientStreamingError::Send).await?;
        let send = UpdateSink::<S, C, M>(send, PhantomData);
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
        &self,
        msg: M,
    ) -> result::Result<
        (
            UpdateSink<S, C, M>,
            BoxStream<'static, result::Result<M::Response, BidiItemError<C>>>,
        ),
        BidiError<C>,
    >
    where
        M: Msg<S, Pattern = BidiStreaming> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, recv) = self.open_bi().await.map_err(BidiError::Open)?;
        send.send(msg).await.map_err(BidiError::Send)?;
        let send = UpdateSink(send, PhantomData);
        let recv = recv
            .map(|x| match x {
                Ok(x) => M::Response::try_from(x).map_err(|_| BidiItemError::DowncastError),
                Err(e) => Err(BidiItemError::RecvError(e)),
            })
            .boxed();
        Ok((send, recv))
    }
}

/// Client error. All client DSL methods return a `Result` with this error type.
#[derive(Debug)]
pub enum RpcClientError<C: ChannelTypes> {
    /// Unable to open a stream to the server
    Open(ClientOpenBiError<C>),
    /// Unable to send the request to the server
    Send(C::SendError),
    /// Server closed the stream before sending a response
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ChannelTypes> fmt::Display for RpcClientError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for RpcClientError<C> {}

/// Server error when accepting a bidi request
#[derive(Debug)]
pub enum BidiError<C: ChannelTypes> {
    /// Unable to open a stream to the server
    Open(ClientOpenBiError<C>),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ChannelTypes> fmt::Display for BidiError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for BidiError<C> {}

/// Server error when receiving an item for a bidi request
#[derive(Debug)]
pub enum BidiItemError<C: ChannelTypes> {
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ChannelTypes> fmt::Display for BidiItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for BidiItemError<C> {}

/// Server error when accepting a client streaming request
#[derive(Debug)]
pub enum ClientStreamingError<C: ChannelTypes> {
    /// Unable to open a stream to the server
    Open(ClientOpenBiError<C>),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ChannelTypes> fmt::Display for ClientStreamingError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for ClientStreamingError<C> {}

/// Server error when receiving an item for a client streaming request
#[derive(Debug)]
pub enum ClientStreamingItemError<C: ChannelTypes> {
    /// Connection was closed before receiving the first message
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ChannelTypes> fmt::Display for ClientStreamingItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for ClientStreamingItemError<C> {}

/// Server error when accepting a server streaming request
#[derive(Debug)]
pub enum StreamingResponseError<C: ChannelTypes> {
    /// Unable to open a stream to the server
    Open(ClientOpenBiError<C>),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ChannelTypes> fmt::Display for StreamingResponseError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for StreamingResponseError<C> {}

/// Client error when handling responses from a server streaming request
#[derive(Debug)]
pub enum StreamingResponseItemError<C: ChannelTypes> {
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ChannelTypes> fmt::Display for StreamingResponseItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ChannelTypes> error::Error for StreamingResponseItemError<C> {}

/// Wrap a stream with an additional item that is kept alive until the stream is dropped
#[pin_project]
struct DeferDrop<S: Stream, X>(#[pin] S, X);

impl<S: Stream, X> Stream for DeferDrop<S, X> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next(cx)
    }
}
