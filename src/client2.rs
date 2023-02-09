//! [RpcClient] and support types
//!
//! This defines the RPC client DSL
use crate::{
    message::{BidiStreaming, ClientStreaming, Msg, Rpc, ServerStreaming},
    RpcError, Service,
};
use futures::{
    future::BoxFuture, stream::BoxStream, Future, FutureExt, Sink, SinkExt, Stream, StreamExt,
    TryFutureExt,
};
use pin_project::pin_project;
use std::{
    error,
    fmt::{self, Debug},
    marker::PhantomData,
    pin::Pin,
    result,
    task::{Context, Poll},
};

/// Errors that can happen when creating and using a substream
pub trait SubstreamErrors: Debug + Clone {
    /// Error when sending messages
    type SendError: RpcError;
    /// Error when receiving messages
    type RecvError: RpcError;
    /// Error when opening a substream
    type OpenError: RpcError;
}

/// A soucre of substreams
///
/// Both the server and the client can be thought as a source of substreams.
/// On the client, acquiring substreams means opening connections.
/// On the server, acquiring substreams means accepting connections.
pub trait SubstreamSource<In, Out>: SubstreamErrors {
    type RecvStream: Stream<Item = Result<In, Self::RecvError>> + Send + Unpin + 'static;
    type SendSink: Sink<Out, Error = Self::SendError> + Send + Unpin + 'static;
    type SubstreamFut<'a>: Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
        + 'a
    where
        Self: 'a;
    fn next(&self) -> Self::SubstreamFut<'_>;
}

/// A client for a specific service
///
/// This is a wrapper around a [SubstreamSource] that serves as the entry point for the client DSL.
/// `S` is the service type, `SS` is the substream source.
#[derive(Debug)]
pub struct RpcClient<S, SS> {
    source: SS,
    p: PhantomData<S>,
}

impl<S: Service, C: SubstreamSource<S::Res, S::Req>> Clone for RpcClient<S, C> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            p: PhantomData,
        }
    }
}

/// Sink that can be used to send updates to the server for the two interaction patterns
/// that support it, [ClientStreaming] and [BidiStreaming].
#[pin_project]
#[derive(Debug)]
pub struct UpdateSink<S: Service, C: SubstreamSource<S::Res, S::Req>, M: Msg<S>>(
    #[pin] C::SendSink,
    PhantomData<M>,
);

impl<S: Service, C: SubstreamSource<S::Res, S::Req>, M: Msg<S>> Sink<M::Update>
    for UpdateSink<S, C, M>
{
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

impl<S: Service, C: SubstreamSource<S::Res, S::Req>> RpcClient<S, C> {
    /// Create a new rpc client from a channel
    pub fn new(source: C) -> Self {
        Self {
            source,
            p: PhantomData,
        }
    }

    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&self, msg: M) -> result::Result<M::Response, RpcClientError<C>>
    where
        M: Msg<S, Pattern = Rpc> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.source.next().await.map_err(RpcClientError::Open)?;
        send.send(msg).await.map_err(RpcClientError::<C>::Send)?;
        let res = recv
            .next()
            .await
            .ok_or(RpcClientError::<C>::EarlyClose)?
            .map_err(RpcClientError::<C>::RecvError)?;
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
        let (mut send, recv) = self
            .source
            .next()
            .await
            .map_err(StreamingResponseError::Open)?;
        send.send(msg)
            .map_err(StreamingResponseError::<C>::Send)
            .await?;
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
        let (mut send, mut recv) = self
            .source
            .next()
            .await
            .map_err(ClientStreamingError::Open)?;
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
        let (mut send, recv) = self.source.next().await.map_err(BidiError::Open)?;
        send.send(msg).await.map_err(BidiError::<C>::Send)?;
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
pub enum RpcClientError<C: SubstreamErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
    /// Server closed the stream before sending a response
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: SubstreamErrors> fmt::Display for RpcClientError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: SubstreamErrors> error::Error for RpcClientError<C> {}

/// Server error when accepting a bidi request
#[derive(Debug)]
pub enum BidiError<C: SubstreamErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: SubstreamErrors> fmt::Display for BidiError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: SubstreamErrors> error::Error for BidiError<C> {}

/// Server error when receiving an item for a bidi request
#[derive(Debug)]
pub enum BidiItemError<C: SubstreamErrors> {
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: SubstreamErrors> fmt::Display for BidiItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: SubstreamErrors> error::Error for BidiItemError<C> {}

/// Server error when accepting a client streaming request
#[derive(Debug)]
pub enum ClientStreamingError<C: SubstreamErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: SubstreamErrors> fmt::Display for ClientStreamingError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: SubstreamErrors> error::Error for ClientStreamingError<C> {}

/// Server error when receiving an item for a client streaming request
#[derive(Debug)]
pub enum ClientStreamingItemError<C: SubstreamErrors> {
    /// Connection was closed before receiving the first message
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: SubstreamErrors> fmt::Display for ClientStreamingItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: SubstreamErrors> error::Error for ClientStreamingItemError<C> {}

/// Server error when accepting a server streaming request
#[derive(Debug)]
pub enum StreamingResponseError<C: SubstreamErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<S: SubstreamErrors> fmt::Display for StreamingResponseError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: SubstreamErrors> error::Error for StreamingResponseError<S> {}

/// Client error when handling responses from a server streaming request
#[derive(Debug)]
pub enum StreamingResponseItemError<S: SubstreamErrors> {
    /// Unable to receive the response from the server
    RecvError(S::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<S: SubstreamErrors> fmt::Display for StreamingResponseItemError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: SubstreamErrors> error::Error for StreamingResponseItemError<S> {}

/// Wrap a stream with an additional item that is kept alive until the stream is dropped
#[pin_project]
struct DeferDrop<S: Stream, X>(#[pin] S, X);

impl<S: Stream, X> Stream for DeferDrop<S, X> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next(cx)
    }
}