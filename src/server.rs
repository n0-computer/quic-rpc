//! Server side api
//!
//! The main entry point is [RpcServer]
use crate::{
    transport::{
        self,
        boxed::BoxableListener,
        mapped::{ErrorOrMapError, MappedRecvStream, MappedSendSink, MappedStreamTypes},
        ConnectionErrors, StreamTypes,
    },
    Listener, RpcMessage, Service,
};
use futures_lite::{Future, Stream, StreamExt};
use futures_util::{SinkExt, TryStreamExt};
use pin_project::pin_project;
use std::{
    error,
    fmt::{self, Debug},
    marker::PhantomData,
    pin::Pin,
    result,
    task::{self, Poll},
};
use tokio::sync::oneshot;

/// Stream types on the server side
///
/// On the server side, we receive requests and send responses.
/// On the client side, we send requests and receive responses.
pub trait ChannelTypes<S: Service>: transport::StreamTypes<In = S::Req, Out = S::Res> {}

impl<T: transport::StreamTypes<In = S::Req, Out = S::Res>, S: Service> ChannelTypes<S> for T {}

/// Type alias for when you want to require a boxed channel
pub type BoxedChannelTypes<S> = crate::transport::boxed::BoxedStreamTypes<
    <S as crate::Service>::Req,
    <S as crate::Service>::Res,
>;

/// Type alias for a service endpoint
pub type BoxedListener<S> =
    crate::transport::boxed::BoxedListener<<S as crate::Service>::Req, <S as crate::Service>::Res>;

/// A server for a specific service.
///
/// This is a wrapper around a [`Listener`] that serves as the entry point for the server DSL.
///
/// Type parameters:
///
/// `S` is the service type.
/// `C` is the channel type.
#[derive(Debug)]
pub struct RpcServer<S, C = BoxedListener<S>> {
    /// The channel on which new requests arrive.
    ///
    /// Each new request is a receiver and channel pair on which messages for this request
    /// are received and responses sent.
    source: C,
    _p: PhantomData<S>,
}

impl<S, C: Clone> Clone for RpcServer<S, C> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            _p: PhantomData,
        }
    }
}

impl<S: Service, C: Listener<S>> RpcServer<S, C> {
    /// Create a new rpc server for a specific service for a [Service] given a compatible
    /// [Listener].
    ///
    /// This is where a generic typed endpoint is converted into a server for a specific service.
    pub fn new(source: C) -> Self {
        Self {
            source,
            _p: PhantomData,
        }
    }

    /// Box the transport for the service.
    ///
    /// The boxed transport is the default for the `C` type parameter, so by boxing we can avoid
    /// having to specify the type parameter.
    pub fn boxed(self) -> RpcServer<S, BoxedListener<S>>
    where
        C: BoxableListener<S::Req, S::Res>,
    {
        RpcServer::new(self.source.boxed())
    }
}

/// A channel for requests and responses for a specific service.
///
/// This just groups the sink and stream into a single type, and attaches the
/// information about the service type.
///
/// Sink and stream are independent, so you can take the channel apart and use
/// them independently.
///
/// Type parameters:
///
/// `S` is the service type.
/// `C` is the service endpoint from which the channel was created.
#[derive(Debug)]
pub struct RpcChannel<S: Service, C: ChannelTypes<S> = BoxedChannelTypes<S>> {
    /// Sink to send responses to the client.
    pub send: C::SendSink,
    /// Stream to receive requests from the client.
    pub recv: C::RecvStream,

    pub(crate) _p: PhantomData<S>,
}

impl<S, C> RpcChannel<S, C>
where
    S: Service,
    C: StreamTypes<In = S::Req, Out = S::Res>,
{
    /// Create a new RPC channel.
    pub fn new(send: C::SendSink, recv: C::RecvStream) -> Self {
        Self {
            send,
            recv,
            _p: PhantomData,
        }
    }

    /// Convert this channel into a boxed channel.
    pub fn boxed(self) -> RpcChannel<S, BoxedChannelTypes<S>>
    where
        C::SendError: Into<anyhow::Error> + Send + Sync + 'static,
        C::RecvError: Into<anyhow::Error> + Send + Sync + 'static,
    {
        let send =
            transport::boxed::SendSink::boxed(Box::new(self.send.sink_map_err(|e| e.into())));
        let recv = transport::boxed::RecvStream::boxed(Box::new(self.recv.map_err(|e| e.into())));
        RpcChannel::new(send, recv)
    }

    /// Map this channel's service into an inner service.
    ///
    /// This method is available if the required bounds are upheld:
    /// SNext::Req: Into<S::Req> + TryFrom<S::Req>,
    /// SNext::Res: Into<S::Res> + TryFrom<S::Res>,
    ///
    /// Where SNext is the new service to map to and S is the current inner service.
    ///
    /// This method can be chained infintely.
    pub fn map<SNext>(self) -> RpcChannel<SNext, MappedStreamTypes<SNext::Req, SNext::Res, C>>
    where
        SNext: Service,
        SNext::Req: TryFrom<S::Req>,
        S::Res: From<SNext::Res>,
    {
        RpcChannel::new(
            MappedSendSink::new(self.send),
            MappedRecvStream::new(self.recv),
        )
    }
}

/// The result of accepting a new connection.
pub struct Accepting<S: Service, C: Listener<S>> {
    send: C::SendSink,
    recv: C::RecvStream,
    _p: PhantomData<S>,
}

impl<S: Service, C: Listener<S>> Accepting<S, C> {
    /// Read the first message from the client.
    ///
    /// The return value is a tuple of `(request, channel)`.  Here `request` is the
    /// first request which is already read from the stream.  The `channel` is a
    /// [RpcChannel] that has `sink` and `stream` fields that can be used to send more
    /// requests and/or receive more responses.
    ///
    /// Often sink and stream will wrap an an underlying byte stream. In this case you can
    /// call into_inner() on them to get it back to perform byte level reads and writes.
    pub async fn read_first(self) -> result::Result<(S::Req, RpcChannel<S, C>), RpcServerError<C>> {
        let Accepting { send, mut recv, .. } = self;
        // get the first message from the client. This will tell us what it wants to do.
        let request: S::Req = recv
            .next()
            .await
            // no msg => early close
            .ok_or(RpcServerError::EarlyClose)?
            // recv error
            .map_err(RpcServerError::RecvError)?;
        Ok((request, RpcChannel::<S, C>::new(send, recv)))
    }
}

impl<S: Service, C: Listener<S>> RpcServer<S, C> {
    /// Accepts a new channel from a client. The result is an [Accepting] object that
    /// can be used to read the first request.
    pub async fn accept(&self) -> result::Result<Accepting<S, C>, RpcServerError<C>> {
        let (send, recv) = self.source.accept().await.map_err(RpcServerError::Accept)?;
        Ok(Accepting {
            send,
            recv,
            _p: PhantomData,
        })
    }

    /// Get the underlying service endpoint
    pub fn into_inner(self) -> C {
        self.source
    }
}

impl<S: Service, C: Listener<S>> AsRef<C> for RpcServer<S, C> {
    fn as_ref(&self) -> &C {
        &self.source
    }
}

/// A stream of updates
///
/// If there is any error with receiving or with decoding the updates, the stream will stall and the error will
/// cause a termination of the RPC call.
#[pin_project]
#[derive(Debug)]
pub struct UpdateStream<C, T>(
    #[pin] C::RecvStream,
    Option<oneshot::Sender<RpcServerError<C>>>,
    PhantomData<T>,
)
where
    C: StreamTypes;

impl<C, T> UpdateStream<C, T>
where
    C: StreamTypes,
    T: TryFrom<C::In>,
{
    pub(crate) fn new(recv: C::RecvStream) -> (Self, UnwrapToPending<RpcServerError<C>>) {
        let (error_send, error_recv) = oneshot::channel();
        let error_recv = UnwrapToPending(error_recv);
        (Self(recv, Some(error_send), PhantomData), error_recv)
    }
}

impl<C, T> Stream for UpdateStream<C, T>
where
    C: StreamTypes,
    T: TryFrom<C::In>,
{
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        let mut this = self.project();
        match Pin::new(&mut this.0).poll_next(cx) {
            Poll::Ready(Some(msg)) => match msg {
                Ok(msg) => {
                    let msg = T::try_from(msg).map_err(|_cause| ());
                    match msg {
                        Ok(msg) => Poll::Ready(Some(msg)),
                        Err(_cause) => {
                            // we were unable to downcast, so we need to send an error
                            if let Some(tx) = this.1.take() {
                                let _ = tx.send(RpcServerError::UnexpectedUpdateMessage);
                            }
                            Poll::Pending
                        }
                    }
                }
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
    Accept(C::AcceptError),
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

impl<In: RpcMessage, Out: RpcMessage, C: ConnectionErrors>
    RpcServerError<MappedStreamTypes<In, Out, C>>
{
    /// For a mapped connection, map the error back to the original error type
    pub fn map_back(self) -> RpcServerError<C> {
        match self {
            RpcServerError::EarlyClose => RpcServerError::EarlyClose,
            RpcServerError::UnexpectedStartMessage => RpcServerError::UnexpectedStartMessage,
            RpcServerError::UnexpectedUpdateMessage => RpcServerError::UnexpectedUpdateMessage,
            RpcServerError::SendError(x) => RpcServerError::SendError(x),
            RpcServerError::Accept(x) => RpcServerError::Accept(x),
            RpcServerError::RecvError(ErrorOrMapError::Inner(x)) => RpcServerError::RecvError(x),
            RpcServerError::RecvError(ErrorOrMapError::Conversion) => {
                RpcServerError::UnexpectedUpdateMessage
            }
        }
    }
}

impl<C: ConnectionErrors> RpcServerError<C> {
    /// Convert into a different error type provided the send, recv and accept errors can be converted
    pub fn errors_into<T>(self) -> RpcServerError<T>
    where
        T: ConnectionErrors,
        C::SendError: Into<T::SendError>,
        C::RecvError: Into<T::RecvError>,
        C::AcceptError: Into<T::AcceptError>,
    {
        match self {
            RpcServerError::EarlyClose => RpcServerError::EarlyClose,
            RpcServerError::UnexpectedStartMessage => RpcServerError::UnexpectedStartMessage,
            RpcServerError::UnexpectedUpdateMessage => RpcServerError::UnexpectedUpdateMessage,
            RpcServerError::SendError(x) => RpcServerError::SendError(x.into()),
            RpcServerError::Accept(x) => RpcServerError::Accept(x.into()),
            RpcServerError::RecvError(x) => RpcServerError::RecvError(x.into()),
        }
    }
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
pub(crate) struct UnwrapToPending<T>(oneshot::Receiver<T>);

impl<T> Future for UnwrapToPending<T> {
    type Output = T;

    fn poll(mut self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Ready(Ok(x)) => Poll::Ready(x),
            Poll::Ready(Err(_)) => Poll::Pending,
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) async fn race2<T, A: Future<Output = T>, B: Future<Output = T>>(f1: A, f2: B) -> T {
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
    C: Listener<S>,
    T: Clone + Send + 'static,
    F: FnMut(RpcChannel<S, C>, S::Req, T) -> Fut + Send + 'static,
    Fut: Future<Output = Result<(), RpcServerError<C>>> + Send + 'static,
{
    let server: RpcServer<S, C> = RpcServer::<S, C>::new(conn);
    loop {
        let (req, chan) = server.accept().await?.read_first().await?;
        let target = target.clone();
        handler(chan, req, target).await?;
    }
}
