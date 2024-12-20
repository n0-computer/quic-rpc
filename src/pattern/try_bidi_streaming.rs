//! Fallible server streaming interaction pattern.

use std::{
    error,
    fmt::{self, Debug},
    result,
};

use futures_lite::{Future, Stream, StreamExt};
use futures_util::{FutureExt, SinkExt, TryFutureExt};
use serde::{Deserialize, Serialize};

use crate::{
    client::{BoxStreamSync, UpdateSink},
    message::{InteractionPattern, Msg},
    server::{race2, RpcChannel, RpcServerError, UpdateStream},
    transport::{self, ConnectionErrors, StreamTypes},
    Connector, RpcClient, Service,
};

/// A guard message to indicate that the stream has been created.
///
/// This is so we can dinstinguish between an error creating the stream and
/// an error in the first item produced by the stream.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StreamCreated;

/// Fallible server streaming interaction pattern.
#[derive(Debug, Clone, Copy)]
pub struct TryBidiStreaming;

impl InteractionPattern for TryBidiStreaming {}

/// Same as ServerStreamingMsg, but with lazy stream creation and the error type explicitly defined.
pub trait TryBidiStreamingMsg<S: Service>: Msg<S, Pattern = TryBidiStreaming>
where
    result::Result<Self::Item, Self::ItemError>: Into<S::Res> + TryFrom<S::Res>,
    result::Result<StreamCreated, Self::CreateError>: Into<S::Res> + TryFrom<S::Res>,
{
    /// Error when creating the stream
    type CreateError: Debug + Send + 'static;

    /// Update type
    type Update: Into<S::Req> + TryFrom<S::Req> + Send + 'static;

    /// Successful response item
    type Item: Send + 'static;

    /// Error for stream items
    type ItemError: Debug + Send + 'static;
}

/// Server error when accepting a server streaming request
///
/// This combines network errors with application errors. Usually you don't
/// care about the exact nature of the error, but if you want to handle
/// application errors differently, you can match on this enum.
#[derive(Debug)]
pub enum Error<C: transport::Connector, E: Debug> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
    /// Error received when creating the stream
    Recv(C::RecvError),
    /// Connection was closed before receiving the first message
    EarlyClose,
    /// Unexpected response from the server
    Downcast,
    /// Application error
    Application(E),
}

impl<S: transport::Connector, E: Debug> fmt::Display for Error<S, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: transport::Connector, E: Debug> error::Error for Error<S, E> {}

/// Client error when handling responses from a server streaming request.
///
/// This combines network errors with application errors.
#[derive(Debug)]
pub enum ItemError<S: ConnectionErrors, E: Debug> {
    /// Unable to receive the response from the server
    Recv(S::RecvError),
    /// Unexpected response from the server
    Downcast,
    /// Application error
    Application(E),
}

impl<S: ConnectionErrors, E: Debug> fmt::Display for ItemError<S, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: ConnectionErrors, E: Debug> error::Error for ItemError<S, E> {}

impl<S, C> RpcChannel<S, C>
where
    C: StreamTypes<In = S::Req, Out = S::Res>,
    S: Service,
{
    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    ///
    /// Compared to [RpcChannel::server_streaming], with this method the stream creation is via
    /// a function that returns a future that resolves to a stream.
    pub async fn try_bidi_streaming<M, F, Fut, Str, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: TryBidiStreamingMsg<S>,
        Result<M::Item, M::ItemError>: Into<S::Res> + TryFrom<S::Res>,
        Result<StreamCreated, M::CreateError>: Into<S::Res> + TryFrom<S::Res>,
        F: FnOnce(T, M, UpdateStream<C, M::Update>) -> Fut + Send + 'static,
        Fut: Future<Output = Result<Str, M::CreateError>> + Send + 'static,
        Str: Stream<Item = Result<M::Item, M::ItemError>> + Send + 'static,
        T: Send + 'static,
    {
        let Self { mut send, recv, .. } = self;
        let (updates, read_error) = UpdateStream::new(recv);
        race2(read_error.map(Err), async move {
            // get the response
            let responses = match f(target, req, updates).await {
                Ok(responses) => {
                    // turn into a S::Res so we can send it
                    let response = Ok(StreamCreated).into();
                    // send it and return the error if any
                    send.send(response)
                        .await
                        .map_err(RpcServerError::SendError)?;
                    responses
                }
                Err(cause) => {
                    // turn into a S::Res so we can send it
                    let response = Err(cause).into();
                    // send it and return the error if any
                    send.send(response)
                        .await
                        .map_err(RpcServerError::SendError)?;
                    return Ok(());
                }
            };
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

impl<S, C> RpcClient<S, C>
where
    C: Connector<S>,
    S: Service,
{
    /// Bidi call to the server, request opens a stream, response is a stream
    pub async fn try_bidi_streaming<M>(
        &self,
        msg: M,
    ) -> result::Result<
        (
            BoxStreamSync<'static, Result<M::Item, ItemError<C, M::ItemError>>>,
            UpdateSink<C, M::Update>,
        ),
        Error<C, M::CreateError>,
    >
    where
        M: TryBidiStreamingMsg<S>,
        Result<M::Item, M::ItemError>: Into<S::Res> + TryFrom<S::Res>,
        Result<StreamCreated, M::CreateError>: Into<S::Res> + TryFrom<S::Res>,
    {
        let msg = msg.into();
        let (mut send, mut recv) = self.source.open().await.map_err(Error::Open)?;
        send.send(msg).map_err(Error::Send).await?;
        let Some(initial) = recv.next().await else {
            return Err(Error::EarlyClose);
        };
        let initial = initial.map_err(Error::Recv)?; // initial response
        let initial = <std::result::Result<StreamCreated, M::CreateError>>::try_from(initial)
            .map_err(|_| Error::Downcast)?;
        let _ = initial.map_err(Error::Application)?;
        let recv = recv.map(move |x| {
            let x = x.map_err(ItemError::Recv)?;
            let x = <std::result::Result<M::Item, M::ItemError>>::try_from(x)
                .map_err(|_| ItemError::Downcast)?;
            let x = x.map_err(ItemError::Application)?;
            Ok(x)
        });
        // keep send alive so the request on the server side does not get cancelled
        let us = UpdateSink::new(send);
        let recv = Box::pin(recv);
        Ok((recv, us))
    }
}
