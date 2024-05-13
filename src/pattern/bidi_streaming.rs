//! Bidirectional stream interaction pattern.

use futures_lite::{Stream, StreamExt};
use futures_util::{FutureExt, SinkExt};

use crate::{
    client::{BoxStreamSync, UpdateSink},
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

/// Bidirectional streaming interaction pattern
///
/// After the initial request, the client can send updates and the server can
/// send responses.
#[derive(Debug, Clone, Copy)]
pub struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}

/// Defines update type and response type for a bidi streaming message.
pub trait BidiStreamingMsg<S: Service>: Msg<S, Pattern = BidiStreaming> {
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

/// Server error when accepting a bidi request
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

/// Server error when receiving an item for a bidi request
#[derive(Debug)]
pub enum ItemError<C: ConnectionErrors> {
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
    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn bidi<M>(
        &self,
        msg: M,
    ) -> result::Result<
        (
            UpdateSink<S, C, M::Update, SInner>,
            BoxStreamSync<'static, result::Result<M::Response, ItemError<C>>>,
        ),
        Error<C>,
    >
    where
        M: BidiStreamingMsg<SInner>,
    {
        let msg = self.map.req_into_outer(msg.into());
        let (mut send, recv) = self.source.open_bi().await.map_err(Error::Open)?;
        send.send(msg).await.map_err(Error::<C>::Send)?;
        let send = UpdateSink(send, PhantomData, Arc::clone(&self.map));
        let map = Arc::clone(&self.map);
        let recv = Box::pin(recv.map(move |x| match x {
            Ok(x) => {
                let x = map
                    .res_try_into_inner(x)
                    .map_err(|_| ItemError::DowncastError)?;
                M::Response::try_from(x).map_err(|_| ItemError::DowncastError)
            }
            Err(e) => Err(ItemError::RecvError(e)),
        }));
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
    pub async fn bidi_streaming<M, F, Str, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: BidiStreamingMsg<SInner>,
        F: FnOnce(T, M, UpdateStream<S, C, M::Update, SInner>) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let Self { mut send, recv, .. } = self;
        // downcast the updates
        let (updates, read_error) = UpdateStream::new(recv, Arc::clone(&self.map));
        // get the response
        let responses = f(target, req, updates);
        race2(read_error.map(Err), async move {
            tokio::pin!(responses);
            while let Some(response) = responses.next().await {
                // turn into a S::Res so we can send it
                let response = self.map.res_into_outer(response.into());
                // send it and return the error if any
                send.send(response)
                    .await
                    .map_err(RpcServerError::SendError)?;
            }
            Ok(())
        })
        .await
    }
}
