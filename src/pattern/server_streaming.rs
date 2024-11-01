//! Server streaming interaction pattern.

use futures_lite::{Stream, StreamExt};
use futures_util::{FutureExt, SinkExt, TryFutureExt};

use crate::{
    client::{BoxStreamSync, DeferDrop},
    message::{InteractionPattern, Msg},
    server::{race2, RpcChannel, RpcServerError},
    transport::{ConnectionCommon, ConnectionErrors},
    RpcClient, Service, ServiceConnection,
};

use std::{
    error,
    fmt::{self, Debug},
    result,
    sync::Arc,
};

/// Server streaming interaction pattern
///
/// After the initial request, the server can send a stream of responses.
#[derive(Debug, Clone, Copy)]
pub struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}

/// Defines response type for a server streaming message.
pub trait ServerStreamingMsg<S: Service>: Msg<S, Pattern = ServerStreaming> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// Server error when accepting a server streaming request
#[derive(Debug)]
pub enum Error<C: ConnectionErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<S: ConnectionErrors> fmt::Display for Error<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: ConnectionErrors> error::Error for Error<S> {}

/// Client error when handling responses from a server streaming request
#[derive(Debug)]
pub enum ItemError<S: ConnectionErrors> {
    /// Unable to receive the response from the server
    RecvError(S::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<S: ConnectionErrors> fmt::Display for ItemError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: ConnectionErrors> error::Error for ItemError<S> {}

impl<S, C, SC> RpcClient<S, C, SC>
where
    SC: Service,
    C: ServiceConnection<SC>,
    S: Service,
{
    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn server_streaming<M>(
        &self,
        msg: M,
    ) -> result::Result<BoxStreamSync<'static, result::Result<M::Response, ItemError<C>>>, Error<C>>
    where
        M: ServerStreamingMsg<S>,
    {
        let msg = self.map.req_into_outer(msg.into());
        let (mut send, recv) = self.source.open().await.map_err(Error::Open)?;
        send.send(msg).map_err(Error::<C>::Send).await?;
        let map = Arc::clone(&self.map);
        let recv = recv.map(move |x| match x {
            Ok(x) => {
                let x = map
                    .res_try_into_inner(x)
                    .map_err(|_| ItemError::DowncastError)?;
                M::Response::try_from(x).map_err(|_| ItemError::DowncastError)
            }
            Err(e) => Err(ItemError::RecvError(e)),
        });
        // keep send alive so the request on the server side does not get cancelled
        let recv = Box::pin(DeferDrop(recv, send));
        Ok(recv)
    }
}

impl<S, C> RpcChannel<S, C>
where
    S: Service,
    C: ConnectionCommon<In = S::Req, Out = S::Res>,
{
    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn server_streaming<M, F, Str, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: ServerStreamingMsg<S>,
        F: FnOnce(T, M) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let Self {
            mut send, mut recv, ..
        } = self;
        // cancel if we get an update, no matter what it is
        let cancel = recv
            .next()
            .map(|_| RpcServerError::UnexpectedUpdateMessage::<C>);
        // race the computation and the cancellation
        race2(cancel.map(Err), async move {
            // get the response
            let responses = f(target, req);
            tokio::pin!(responses);
            while let Some(response) = responses.next().await {
                // turn into a S::Res so we can send it
                let response = response.into();
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
