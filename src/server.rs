//! [RpcServer] and support types
//!
//! This defines the RPC server DSL
use crate::{
    message::{BidiStreaming, ClientStreaming, Msg, Rpc, ServerStreaming},
    ConnectionErrors, Service, ServiceEndpoint,
};
use futures::{channel::oneshot, task, task::Poll, Future, FutureExt, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use std::{error, fmt, fmt::Debug, marker::PhantomData, pin::Pin, result};

/// A server channel for a specific service.
///
/// This is a wrapper around a [crate::ServerChannel] that serves as the entry point for the server DSL.
/// `S` is the service type, `C` is the channel type.
#[derive(Debug)]
pub struct RpcServer<S, C> {
    /// The channel on which new requests arrive.
    ///
    /// Each new request is a receiver and channel pair on which messages for this request
    /// are received and responses sent.
    source: C,
    p: PhantomData<S>,
}

impl<S, C: Clone> Clone for RpcServer<S, C> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            p: PhantomData,
        }
    }
}

impl<S: Service, C: ConnectionErrors> RpcServer<S, C> {
    /// Create a new server channel from a channel and a service type.
    pub fn new(source: C) -> Self {
        Self {
            source,
            p: PhantomData,
        }
    }
}

pub struct RpcChannel<S: Service, C: ServiceEndpoint<S>> {
    pub send: C::SendSink,
    pub recv: C::RecvStream,
    p: PhantomData<S>,
}

impl<S: Service, C: ServiceEndpoint<S>> RpcChannel<S, C> {
    pub fn new(send: C::SendSink, recv: C::RecvStream) -> Self {
        Self {
            send,
            recv,
            p: PhantomData,
        }
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn rpc<M, F, Fut, T>(
        self,
        req: M,
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = Rpc>,
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
            let res: S::Res = res.into();
            // send it and return the error if any
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }

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
        M: Msg<S, Pattern = ClientStreaming>,
        F: FnOnce(T, M, UpdateStream<S, C, M>) -> Fut + Send + 'static,
        Fut: Future<Output = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let Self { mut send, recv, .. } = self;
        let (updates, read_error) = UpdateStream::new(recv);
        race2(read_error.map(Err), async move {
            // get the response
            let res = f(target, req, updates).await;
            // turn into a S::Res so we can send it
            let res: S::Res = res.into();
            // send it and return the error if any
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
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = BidiStreaming>,
        F: FnOnce(T, M, UpdateStream<S, C, M>) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let Self { mut send, recv, .. } = self;
        // downcast the updates
        let (updates, read_error) = UpdateStream::new(recv);
        // get the response
        let responses = f(target, req, updates);
        race2(read_error.map(Err), async move {
            tokio::pin!(responses);
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
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = ServerStreaming>,
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
        M: Msg<S, Pattern = Rpc, Response = result::Result<R, E2>>,
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

impl<S: Service, C: ServiceEndpoint<S>> RpcServer<S, C> {
    /// Accepts a connection, handling the first request.
    ///
    /// This accepts a new client connection, which is represented as a tuple of a sender
    /// [`Sink`] and receiver [`Stream`] with `Item`s being the [`Service::Req`] and
    /// [`Service::Res`] respectively.
    ///
    /// The return value is a tuple of `(request, (sink, stream))`.  Here `request` is the
    /// first request which is already read from the stream.  The `sink` and `stream` are
    /// used to send more requests and/or receive more responses.
    ///
    /// Often sink and stream will wrap an an underlying byte stream. In this case you can
    /// call into_inner() on them to get
    ///
    /// [`Sink`]: futures::sink::Sink
    /// [`Stream`]: futures::stream::Stream
    pub async fn accept_one(
        &self,
    ) -> result::Result<(S::Req, RpcChannel<S, C>), RpcServerError<C>> {
        let (send, mut recv) = self
            .source
            .accept_bi()
            .await
            .map_err(RpcServerError::Accept)?;

        // get the first message from the client. This will tell us what it wants to do.
        let request: S::Req = recv
            .next()
            .await
            // no msg => early close
            .ok_or(RpcServerError::EarlyClose)?
            // recv error
            .map_err(RpcServerError::RecvError)?;
        Ok((request, RpcChannel::new(send, recv)))
    }

    /// A rpc call that also maps the error from the user type to the wire type
    ///
    /// This is useful if you want to write your function with a convenient error type like anyhow::Error,
    /// yet still use a serializable error type on the wire.
    pub async fn rpc_map_err<M, F, Fut, T, R, E1, E2>(
        &self,
        req: M,
        chan: (C::SendSink, C::RecvStream),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = Rpc, Response = result::Result<R, E2>>,
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
        self.rpc(req, chan, target, fut).await
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn rpc<M, F, Fut, T>(
        &self,
        req: M,
        chan: (C::SendSink, C::RecvStream),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = Rpc>,
        F: FnOnce(T, M) -> Fut,
        Fut: Future<Output = M::Response>,
        T: Send + 'static,
    {
        let (mut send, mut recv) = chan;
        // cancel if we get an update, no matter what it is
        let cancel = recv
            .next()
            .map(|_| RpcServerError::UnexpectedUpdateMessage::<C>);
        // race the computation and the cancellation
        race2(cancel.map(Err), async move {
            // get the response
            let res = f(target, req).await;
            // turn into a S::Res so we can send it
            let res: S::Res = res.into();
            // send it and return the error if any
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn client_streaming<M, F, Fut, T>(
        &self,
        req: M,
        c: (C::SendSink, C::RecvStream),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = ClientStreaming>,
        F: FnOnce(T, M, UpdateStream<S, C, M>) -> Fut + Send + 'static,
        Fut: Future<Output = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (mut send, recv) = c;
        let (updates, read_error) = UpdateStream::new(recv);
        race2(read_error.map(Err), async move {
            // get the response
            let res = f(target, req, updates).await;
            // turn into a S::Res so we can send it
            let res: S::Res = res.into();
            // send it and return the error if any
            send.send(res).await.map_err(RpcServerError::SendError)
        })
        .await
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn bidi_streaming<M, F, Str, T>(
        &self,
        req: M,
        c: (C::SendSink, C::RecvStream),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = BidiStreaming>,
        F: FnOnce(T, M, UpdateStream<S, C, M>) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (mut send, recv) = c;
        // downcast the updates
        let (updates, read_error) = UpdateStream::new(recv);
        // get the response
        let responses = f(target, req, updates);
        race2(read_error.map(Err), async move {
            tokio::pin!(responses);
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
        &self,
        req: M,
        c: (C::SendSink, C::RecvStream),
        target: T,
        f: F,
    ) -> result::Result<(), RpcServerError<C>>
    where
        M: Msg<S, Pattern = ServerStreaming>,
        F: FnOnce(T, M) -> Str + Send + 'static,
        Str: Stream<Item = M::Response> + Send + 'static,
        T: Send + 'static,
    {
        let (mut send, mut recv) = c;
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
}

/// A stream of updates
///
/// If there is any error with receiving or with decoding the updates, the stream will stall and the error will
/// cause a termination of the RPC call.
#[pin_project]
pub struct UpdateStream<S: Service, C: ServiceEndpoint<S>, M: Msg<S>>(
    #[pin] C::RecvStream,
    Option<oneshot::Sender<RpcServerError<C>>>,
    PhantomData<M>,
);

impl<S: Service, C: ServiceEndpoint<S>, M: Msg<S>> UpdateStream<S, C, M> {
    fn new(recv: C::RecvStream) -> (Self, UnwrapToPending<RpcServerError<C>>) {
        let (error_send, error_recv) = oneshot::channel();
        let error_recv = UnwrapToPending(error_recv);
        (Self(recv, Some(error_send), PhantomData), error_recv)
    }
}

impl<S: Service, C: ServiceEndpoint<S>, M: Msg<S>> Stream for UpdateStream<S, C, M> {
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

/// Server error. All server DSL methods return a `Result` with this error type.
pub enum RpcServerError<C: ConnectionErrors> {
    /// Unable to open a new channel
    Accept(C::OpenError),
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

impl<C: ConnectionErrors> fmt::Debug for RpcServerError<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accept(arg0) => f.debug_tuple("Open").field(arg0).finish(),
            Self::EarlyClose => write!(f, "EarlyClose"),
            Self::RecvError(arg0) => f.debug_tuple("RecvError").field(arg0).finish(),
            Self::SendError(arg0) => f.debug_tuple("SendError").field(arg0).finish(),
            Self::UnexpectedStartMessage => f.debug_tuple("UnexpectedStartMessage").finish(),
            Self::UnexpectedUpdateMessage => f.debug_tuple("UnexpectedStartMessage").finish(),
        }
    }
}

impl<C: ConnectionErrors> fmt::Display for RpcServerError<C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl<C: ConnectionErrors> error::Error for RpcServerError<C> {}

/// Take an oneshot receiver and just return Pending the underlying future returns `Err(oneshot::Canceled)`
struct UnwrapToPending<T>(oneshot::Receiver<T>);

impl<T> Future for UnwrapToPending<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match self.0.poll_unpin(cx) {
            Poll::Ready(Ok(x)) => Poll::Ready(x),
            Poll::Ready(Err(_)) => Poll::Pending,
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

/// Run a server loop, invoking a handler callback for each request.
///
/// Requests will be handled sequentially.
pub async fn run_server_loop<S, C, T, F, Fut>(
    _service_type: S,
    conn: C,
    target: T,
    mut handler: F,
) -> Result<(), RpcServerError<C>>
where
    S: Service,
    C: ServiceEndpoint<S>,
    T: Clone + Send + 'static,
    F: FnMut(RpcChannel<S, C>, S::Req, T) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), RpcServerError<C>>> + Send + 'static,
{
    let server = RpcServer::<S, C>::new(conn);
    loop {
        let (req, chan) = server.accept_one().await?;
        let target = target.clone();
        handler(chan, req, target).await?;
    }
}
