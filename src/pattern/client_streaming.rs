//! Client streaming interaction pattern.

use futures_lite::{future::Boxed, Future, StreamExt};
use futures_util::{FutureExt, SinkExt, TryFutureExt};

use crate::{
    client::UpdateSink,
    message::{InteractionPattern, Msg},
    server::{race2, RpcChannel, RpcServerError, UpdateStream},
    transport::ConnectionErrors,
    RpcClient, Service, ServiceConnection, ServiceEndpoint,
};

use std::{
    error,
    fmt::{self, Debug},
    marker::PhantomData,
    result,
    sync::Arc,
};

/// Client streaming interaction pattern
///
/// After the initial request, the client can send updates, but there is only
/// one response.
#[derive(Debug, Clone, Copy)]
pub struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}

/// Defines update type and response type for a client streaming message.
pub trait ClientStreamingMsg<S: Service>: Msg<S, Pattern = ClientStreaming> {
    /// The type for request updates
    ///
    /// For a request that does not support updates, this can be safely set to any type, including
    /// the message type itself. Any update for such a request will result in an error.
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;

    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Server error when accepting a client streaming request
#[derive(Debug)]
pub enum Error<C: ConnectionErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ConnectionErrors> fmt::Display for Error<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for Error<C> {}

/// Server error when receiving an item for a client streaming request
#[derive(Debug)]
pub enum ItemError<C: ConnectionErrors> {
    /// Connection was closed before receiving the first message
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ConnectionErrors> fmt::Display for ItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for ItemError<C> {}

impl<S, C, SInner> RpcClient<S, C, SInner>
where
    S: Service,
    C: ServiceConnection<S>,
    SInner: Service,
{
    /// Call to the server that allows the client to stream, single response
    pub async fn client_streaming<M>(
        &self,
        msg: M,
    ) -> result::Result<
        (
            UpdateSink<S, C, M::Update, SInner>,
            Boxed<result::Result<M::Response, ItemError<C>>>,
        ),
        Error<C>,
    >
    where
        M: ClientStreamingMsg<SInner>,
    {
        let msg = self.map.req_into_outer(msg.into());
        let (mut send, mut recv) = self.source.open_bi().await.map_err(Error::Open)?;
        send.send(msg).map_err(Error::Send).await?;
        let send = UpdateSink::<S, C, M::Update, SInner>(send, PhantomData, Arc::clone(&self.map));
        let map = Arc::clone(&self.map);
        let recv = async move {
            let item = recv.next().await.ok_or(ItemError::EarlyClose)?;

            match item {
                Ok(x) => {
                    let x = map
                        .res_try_into_inner(x)
                        .map_err(|_| ItemError::DowncastError)?;
                    M::Response::try_from(x).map_err(|_| ItemError::DowncastError)
                }
                Err(e) => Err(ItemError::RecvError(e)),
            }
        }
        .boxed();
        Ok((send, recv))
    }
}

impl<S, C, SInner> RpcChannel<S, C, SInner>
where
    S: Service,
    C: ServiceEndpoint<S>,
    SInner: Service,
{
    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn client_streaming<M, F, Fut, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: ClientStreamingMsg<SInner>,
        F: FnOnce(T, M, UpdateStream<S, C, M::Update, SInner>) -> Fut + Send + 'static,
        Fut: Future<Output = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let Self { mut send, recv, .. } = self;
        let (updates, read_error) = UpdateStream::new(recv, Arc::clone(&self.map));
        race2(read_error.map(Err), async move {
            // get the response
            let res = f(target, req, updates).await;
            // turn into a S::Res so we can send it
            let res = self.map.res_into_outer(res.into());
            // send it and return the error if any
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }
}
