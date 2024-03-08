//! Client side api
//!
//! The main entry point is [RpcClient].
use crate::{
    message::{BidiStreamingMsg, ClientStreamingMsg, RpcMsg, ServerStreamingMsg},
    transport::ConnectionErrors,
    Service, ServiceConnection,
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

/// A client for a specific service
///
/// This is a wrapper around a [ServiceConnection] that serves as the entry point
/// for the client DSL. `S` is the service type, `C` is the substream source.
#[derive(Debug)]
pub struct RpcClient<S, C, Sd = S> {
    source: C,
    map: Arc<dyn IntoService<Up = S, Down = Sd>>,
}

impl<S, C: Clone, S2> Clone for RpcClient<S, C, S2> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            map: Arc::clone(&self.map),
        }
    }
}

/// Sink that can be used to send updates to the server for the two interaction patterns
/// that support it, [crate::message::ClientStreaming] and [crate::message::BidiStreaming].
#[pin_project]
#[derive(Debug)]
pub struct UpdateSink<S, C, T, Sd = S>(#[pin] C::SendSink, Arc<dyn IntoService<Up = S, Down = Sd>>)
where
    S: Service,
    Sd: Service,
    C: ServiceConnection<S>,
    T: Into<Sd::Req>;

impl<S, C, T, Sd> Sink<T> for UpdateSink<S, C, T, Sd>
where
    S: Service,
    Sd: Service,
    C: ServiceConnection<S>,
    T: Into<Sd::Req>,
{
    type Error = C::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let req = self.map.req_up(item);
        self.project().0.start_send_unpin(req)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

impl<S, C, Sd> RpcClient<S, C, Sd>
where
    S: Service,
    C: ServiceConnection<S>,
{
    /// Create a new rpc client for a specific [Service] given a compatible
    /// [ServiceConnection].
    ///
    /// This is where a generic typed connection is converted into a client for a specific service.
    pub fn new(source: C) -> Self {
        Self {
            source,
            map: Arc::new(Mapper::new()),
        }
    }
}

impl<S, C, Sd> RpcClient<S, C, Sd>
where
    S: Service,
    C: ServiceConnection<S>,
    Sd: Service,
{
    /// Get the underlying connection
    pub fn into_inner(self) -> C {
        self.source
    }

    /// Map this channel's service into an inner service.
    ///
    /// This method is available as long as the outer service implements [`IntoService`] for the
    /// inner service.
    pub fn map<Sd2>(self) -> RpcClient<S, C, Sd2>
    where
        Sd2: Service,
        Sd2::Req: Into<Sd::Req> + TryFrom<Sd::Req>,
        Sd2::Res: Into<Sd::Res> + TryFrom<Sd::Req>,
    {
        let mapper = Mapper::<Sd, Sd2>::new();
        let map = ChainedMapper::new(self.map, mapper);
        RpcClient {
            source: self.source,
            map: Arc::new(map),
        }
    }

    /// RPC call to the server, single request, single response
    pub async fn rpc<M>(&self, msg: M) -> result::Result<M::Response, RpcClientError<C>>
    where
        M: RpcMsg<Sd>,
    {
        let msg: Sd::Req = msg.into();
        let msg = self.map.req_up(msg);
        todo!()
        // let msg: Sd::Req = msg.into();
        // let msg = self.map.req_up(msg);
        // let (mut send, mut recv) = self.source.open_bi().await.map_err(RpcClientError::Open)?;
        // send.send(msg).await.map_err(RpcClientError::<C>::Send)?;
        // let res = recv
        //     .next()
        //     .await
        //     .ok_or(RpcClientError::<C>::EarlyClose)?
        //     .map_err(RpcClientError::<C>::RecvError)?;
        // // keep send alive until we have the answer
        // drop(send);
        // let res: S::Res = res;
        // let res: Sd::Res = self
        //     .map
        //     .try_res_down(res)
        //     .map_err(|_| RpcClientError::DowncastError)?;
        // M::Response::try_from(res).map_err(|_| RpcClientError::DowncastError)
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
        M: ServerStreamingMsg<Sd>,
    {
        let msg = self.map.req_up(msg);
        let (mut send, recv) = self
            .source
            .open_bi()
            .await
            .map_err(StreamingResponseError::Open)?;
        send.send(msg)
            .map_err(StreamingResponseError::<C>::Send)
            .await?;
        let recv = recv.map(move |x| match x {
            Ok(x) => {
                let x = self
                    .map
                    .try_res_down(x)
                    .map_err(|_| StreamingResponseItemError::DowncastError)?;
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
            UpdateSink<S, C, M::Update, Sd>,
            BoxFuture<'static, result::Result<M::Response, ClientStreamingItemError<C>>>,
        ),
        ClientStreamingError<C>,
    >
    where
        M: ClientStreamingMsg<Sd>,
    {
        let msg = self.map.req_up(msg);
        let (mut send, mut recv) = self
            .source
            .open_bi()
            .await
            .map_err(ClientStreamingError::Open)?;
        send.send(msg).map_err(ClientStreamingError::Send).await?;
        let send = UpdateSink::<S, C, M::Update, Sd>(send, Arc::clone(&self.map));
        let recv = async move {
            let item = recv
                .next()
                .await
                .ok_or(ClientStreamingItemError::EarlyClose)?;

            match item {
                Ok(x) => {
                    let x = self
                        .map
                        .try_res_down(x)
                        .map_err(|_| ClientStreamingItemError::DowncastError)?;
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
            UpdateSink<S, C, M::Update, Sd>,
            BoxStream<'static, result::Result<M::Response, BidiItemError<C>>>,
        ),
        BidiError<C>,
    >
    where
        M: BidiStreamingMsg<Sd>,
    {
        let msg = self.map.req_up(msg);
        let (mut send, recv) = self.source.open_bi().await.map_err(BidiError::Open)?;
        send.send(msg).await.map_err(BidiError::<C>::Send)?;
        let send = UpdateSink(send, Arc::clone(&self.map));
        let recv = recv
            .map(|x| match x {
                Ok(x) => {
                    let x = self
                        .map
                        .try_res_down(x)
                        .map_err(|_| BidiItemError::DowncastError)?;
                    M::Response::try_from(x).map_err(|_| BidiItemError::DowncastError)
                }
                Err(e) => Err(BidiItemError::RecvError(e)),
            })
            .boxed();
        Ok((send, recv))
    }
}

impl<S: Service, C: ServiceConnection<S>> AsRef<C> for RpcClient<S, C> {
    fn as_ref(&self) -> &C {
        &self.source
    }
}

/// Client error. All client DSL methods return a `Result` with this error type.
#[derive(Debug)]
pub enum RpcClientError<C: ConnectionErrors> {
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

impl<C: ConnectionErrors> fmt::Display for RpcClientError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for RpcClientError<C> {}

/// Server error when accepting a bidi request
#[derive(Debug)]
pub enum BidiError<C: ConnectionErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ConnectionErrors> fmt::Display for BidiError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for BidiError<C> {}

/// Server error when receiving an item for a bidi request
#[derive(Debug)]
pub enum BidiItemError<C: ConnectionErrors> {
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ConnectionErrors> fmt::Display for BidiItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for BidiItemError<C> {}

/// Server error when accepting a client streaming request
#[derive(Debug)]
pub enum ClientStreamingError<C: ConnectionErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<C: ConnectionErrors> fmt::Display for ClientStreamingError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for ClientStreamingError<C> {}

/// Server error when receiving an item for a client streaming request
#[derive(Debug)]
pub enum ClientStreamingItemError<C: ConnectionErrors> {
    /// Connection was closed before receiving the first message
    EarlyClose,
    /// Unable to receive the response from the server
    RecvError(C::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<C: ConnectionErrors> fmt::Display for ClientStreamingItemError<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<C: ConnectionErrors> error::Error for ClientStreamingItemError<C> {}

/// Server error when accepting a server streaming request
#[derive(Debug)]
pub enum StreamingResponseError<C: ConnectionErrors> {
    /// Unable to open a substream at all
    Open(C::OpenError),
    /// Unable to send the request to the server
    Send(C::SendError),
}

impl<S: ConnectionErrors> fmt::Display for StreamingResponseError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: ConnectionErrors> error::Error for StreamingResponseError<S> {}

/// Client error when handling responses from a server streaming request
#[derive(Debug)]
pub enum StreamingResponseItemError<S: ConnectionErrors> {
    /// Unable to receive the response from the server
    RecvError(S::RecvError),
    /// Unexpected response from the server
    DowncastError,
}

impl<S: ConnectionErrors> fmt::Display for StreamingResponseItemError<S> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<S: ConnectionErrors> error::Error for StreamingResponseItemError<S> {}

/// Wrap a stream with an additional item that is kept alive until the stream is dropped
#[pin_project]
struct DeferDrop<S: Stream, X>(#[pin] S, X);

impl<S: Stream, X> Stream for DeferDrop<S, X> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next(cx)
    }
}

trait IntoService: 'static {
    type Up: Service;
    type Down: Service;
    fn req_up(&self, req: impl Into<<Self::Down as Service>::Req>) -> <Self::Up as Service>::Req;
    fn res_up(&self, res: impl Into<<Self::Down as Service>::Res>) -> <Self::Up as Service>::Res;
    fn try_req_down(
        &self,
        req: <Self::Up as Service>::Req,
    ) -> Result<<Self::Down as Service>::Req, ()>;
    fn try_res_down(
        &self,
        res: <Self::Up as Service>::Res,
    ) -> Result<<Self::Down as Service>::Res, ()>;
}

struct Mapper<S1, S2> {
    s1: PhantomData<S1>,
    s2: PhantomData<S2>,
}

impl<S1, S2> Mapper<S1, S2>
where
    S1: Service,
    S2: Service,
    S2::Req: Into<S1::Req>,
    S2::Res: Into<S1::Res>,
{
    pub fn new() -> Self {
        Self {
            s1: PhantomData,
            s2: PhantomData,
        }
    }
}

impl<S1, S2> IntoService for Mapper<S1, S2>
where
    S1: Service,
    S2: Service,
    S2::Req: Into<S1::Req> + TryFrom<S1::Req>,
    S2::Res: Into<S1::Res> + TryFrom<S1::Res>,
{
    type Up = S1;
    type Down = S2;

    fn req_up(&self, req: impl Into<<Self::Down as Service>::Req>) -> <Self::Up as Service>::Req {
        (req.into()).into()
    }

    fn res_up(&self, res: impl Into<<Self::Down as Service>::Res>) -> <Self::Up as Service>::Res {
        (res.into()).into()
    }

    fn try_req_down(
        &self,
        req: <Self::Up as Service>::Req,
    ) -> Result<<Self::Down as Service>::Req, ()> {
        req.try_into().map_err(|_| ())
    }

    fn try_res_down(
        &self,
        res: <Self::Up as Service>::Res,
    ) -> Result<<Self::Down as Service>::Res, ()> {
        res.try_into().map_err(|_| ())
    }
}

struct ChainedMapper<S1, S2, M2>
where
    S1: Service,
    S2: Service,
    M2: IntoService,
    <M2::Up as Service>::Req: Into<<S2 as Service>::Req>,
{
    m1: Arc<dyn IntoService<Up = S1, Down = S2>>,
    m2: M2,
}

impl<S1, S2, M2> ChainedMapper<S1, S2, M2>
where
    S1: Service,
    S2: Service,
    M2: IntoService,
    <M2::Up as Service>::Req: Into<<S2 as Service>::Req>,
{
    ///
    pub fn new(m1: Arc<dyn IntoService<Up = S1, Down = S2>>, m2: M2) -> Self {
        Self { m1, m2 }
    }
}

impl<S1, S2, M2> IntoService for ChainedMapper<S1, S2, M2>
where
    S1: Service,
    S2: Service,
    M2: IntoService,
    <M2::Up as Service>::Req: Into<<S2 as Service>::Req>,
    <M2::Up as Service>::Res: Into<<S2 as Service>::Res>,
{
    type Up = S1;
    type Down = <M2 as IntoService>::Down;
    fn req_up(&self, req: <Self::Down as Service>::Req) -> <Self::Up as Service>::Req {
        let req = self.m2.req_up(req);
        let req = self.m1.req_up(req.into());
        req
    }
    fn res_up(&self, res: <Self::Down as Service>::Res) -> <Self::Up as Service>::Res {
        let res = self.m2.res_up(res);
        let res = self.m1.res_up(res.into());
        res
    }
    fn try_req_down(
        &self,
        req: <Self::Up as Service>::Req,
    ) -> Result<<Self::Down as Service>::Req, ()> {
        let req = self.m2.try_req_down(req);
        let req = self.m1.try_req_down(req.into());
        req
    }

    fn try_res_down(
        &self,
        res: <Self::Up as Service>::Res,
    ) -> Result<<Self::Down as Service>::Res, ()> {
        let res = self.m2.try_res_down(res);
        let res = self.m1.try_res_down(res.into());
        res
    }
}
