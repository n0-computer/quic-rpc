use futures::future;
use futures::future::BoxFuture;
use futures::stream::BoxStream;
use futures::FutureExt;
use futures::Sink;
use futures::SinkExt;
use futures::StreamExt;
use futures::TryFutureExt;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::marker::PhantomData;
use std::pin::Pin;
use std::result;

use crate::mem_and_quinn as underlying;
use crate::Channel;

/// A service
pub trait Service {
    type Req: Serialize + DeserializeOwned + Send + Unpin + 'static;
    type Res: Serialize + DeserializeOwned + Send + Unpin + 'static;
}

/// A message for a service
///
/// For each server and each message, only one interaction pattern can be defined.
pub trait Msg<S: Service>: Into<S::Req> + 'static {
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
    type Pattern: InteractionPattern;
}

pub trait HandleBidi<S: Service, M: Msg<S, Pattern = BidiStreaming>> {
    fn handle(
        &mut self,
        msg: M,
        input: underlying::ResStream<S::Req>,
        output: underlying::ReqSink<S::Res>,
    ) -> BoxFuture<'_, result::Result<(), underlying::SendError>>;
}

pub trait HandleRpc<S: Service, M: Msg<S, Update = NoRequest, Pattern = Rpc>> {
    fn handle(
        &mut self,
        msg: M,
        _input: underlying::ResStream<S::Req>,
        mut output: underlying::ReqSink<S::Res>,
    ) -> BoxFuture<'_, result::Result<(), underlying::SendError>> {
        let fut = self.rpc(msg);
        async move {
            let response = fut.await;
            output.send(response.into()).await
        }
        .boxed()
    }

    fn rpc(&mut self, msg: M) -> BoxFuture<'static, M::Response>;
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

pub struct ClientChannel<S, Req, Res> {
    channel: underlying::Channel<Req, Res>,
    _s: PhantomData<S>,
}

pub enum FireAndForgetError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

/// Error for rpc interactions
pub enum RpcError {
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

pub enum BidiError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

pub enum ClientStreamingError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

pub enum ClientStreamingItemError {
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

pub enum StreamingResponseError {
    /// Unable to open a stream to the server
    Open(underlying::OpenBiError),
    /// Unable to send the request to the server
    Send(underlying::SendError),
}

pub enum StreamingResponseItemError {
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

pub enum BidiItemError {
    /// Unable to receive the response from the server
    RecvError(underlying::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<S: Service> ClientChannel<S, S::Req, S::Res> {
    pub fn new(channel: underlying::Channel<S::Req, S::Res>) -> Self {
        Self {
            channel,
            _s: PhantomData,
        }
    }

    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&mut self, msg: M) -> result::Result<M::Response, RpcError>
    where
        M: Msg<S, Pattern = Rpc> + Into<S::Req>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.channel.open_bi().await.map_err(RpcError::Open)?;
        send.send(msg).await.map_err(RpcError::Send)?;
        let res = recv
            .next()
            .await
            .ok_or(RpcError::EarlyClose)?
            .map_err(RpcError::RecvError)?;
        M::Response::try_from(res).map_err(|_| RpcError::DowncastError)
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
            Pin<Box<dyn Sink<M::Update, Error = underlying::SendError>>>,
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
            Pin<Box<dyn Sink<M::Update, Error = underlying::SendError>>>,
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
