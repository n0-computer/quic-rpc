//! RPC interaction pattern.

use futures_lite::{Future, StreamExt};
use futures_util::{FutureExt, SinkExt};

use crate::{
    message::{InteractionPattern, Msg},
    server::{race2, RpcChannel, RpcServerError},
    transport::ConnectionErrors,
    RpcClient, Service, ServiceConnection, ServiceEndpoint,
};

use std::{
    error,
    fmt::{self, Debug},
    result,
};

/// Rpc interaction pattern
///
/// There is only one request and one response.
#[derive(Debug, Clone, Copy)]
pub struct Rpc;
impl InteractionPattern for Rpc {}

/// Defines the response type for a rpc message.
///
/// Since this is the most common interaction pattern, this also implements [Msg] for you
/// automatically, with the interaction pattern set to [Rpc]. This is to reduce boilerplate
/// when defining rpc messages.
pub trait RpcMsg<S: Service>: Msg<S, Pattern = Rpc> {
    /// The type for the response
    ///
    /// For requests that can produce errors, this can be set to [Result<T, E>](std::result::Result).
    type Response: Into<S::Res> + TryFrom<S::Res> + Send + 'static;
}

/// We can only do this for one trait, so we do it for RpcMsg since it is the most common
impl<T: RpcMsg<S>, S: Service> Msg<S> for T {
    type Pattern = Rpc;
}
/// Client error. All client DSL methods return a `Result` with this error type.
#[derive(Debug)]
pub enum Error<C: ConnectionErrors> {
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

impl<C: ConnectionErrors> fmt::Display for Error<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for Error<C> {}

impl<SInner, S, C> RpcClient<SInner, S, C>
where
    S: Service,
    C: ServiceConnection<S>,
    SInner: Service,
{
    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&self, msg: M) -> result::Result<M::Response, Error<C>>
    where
        M: RpcMsg<SInner>,
    {
        let msg = self.map.req_into_outer(msg.into());
        let (mut send, mut recv) = self.source.open().await.map_err(Error::Open)?;
        send.send(msg).await.map_err(Error::<C>::Send)?;
        let res = recv
            .next()
            .await
            .ok_or(Error::<C>::EarlyClose)?
            .map_err(Error::<C>::RecvError)?;
        // keep send alive until we have the answer
        drop(send);
        let res = self
            .map
            .res_try_into_inner(res)
            .map_err(|_| Error::DowncastError)?;
        M::Response::try_from(res).map_err(|_| Error::DowncastError)
    }
}

impl<S, C, SInner> RpcChannel<S, C, SInner>
where
    S: Service,
    C: ServiceEndpoint<S>,
    SInner: Service,
{
    /// handle the message of type `M` using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn rpc<M, F, Fut, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: RpcMsg<SInner>,
        F: FnOnce(T, M) -> Fut,
        Fut: Future<Output = M::Response>,
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
            let res = f(target, req).await;
            // turn into a S::Res so we can send it
            let res = self.map.res_into_outer(res.into());
            // send it and return the error if any
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }

    /// A rpc call that also maps the error from the user type to the wire type
    ///
    /// This is useful if you want to write your function with a convenient error type like anyhow::Error,
    /// yet still use a serializable error type on the wire.
    pub async fn rpc_map_err<M, F, Fut, T, R, E1, E2>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: RpcMsg<SInner, Response = result::Result<R, E2>>,
        F: FnOnce(T, M) -> Fut,
        Fut: Future<Output = result::Result<R, E1>>,
        E2: From<E1>,
        T: Send + 'static,
    {
        let fut = |target: T, msg: M| async move {
            // call the inner fn
            let res: Result<R, E1> = f(target, msg).await;
            // convert the error type
            let res: Result<R, E2> = res.map_err(E2::from);
            res
        };
        self.rpc(req, target, fut).await
    }
}
