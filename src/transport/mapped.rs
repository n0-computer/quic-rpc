//! Transport with mapped input and output types.
use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
    task::{Context, Poll},
};

use futures_lite::{Stream, StreamExt};
use futures_util::SinkExt;
use pin_project::pin_project;

use crate::{server::RpcChannel, RpcError, RpcMessage, Service};

use super::{Connection, ConnectionCommon, ConnectionErrors};

/// A connection that maps input and output types
#[derive(Debug)]
pub struct MappedConnection<In, Out, C> {
    inner: C,
    _phantom: std::marker::PhantomData<(In, Out)>,
}

impl<In, Out, C> MappedConnection<In, Out, C>
where
    C: Connection,
    In: TryFrom<C::In>,
    C::Out: From<Out>,
{
    /// Create a new mapped connection
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<In, Out, C> Clone for MappedConnection<In, Out, C>
where
    C: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<In, Out, C> ConnectionErrors for MappedConnection<In, Out, C>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ConnectionErrors,
{
    type RecvError = ErrorOrMapError<C::RecvError>;
    type SendError = C::SendError;
    type OpenError = C::OpenError;
    type AcceptError = C::AcceptError;
}

impl<In, Out, C> ConnectionCommon for MappedConnection<In, Out, C>
where
    C: ConnectionCommon,
    In: RpcMessage,
    Out: RpcMessage,
    In: TryFrom<C::In>,
    C::Out: From<Out>,
{
    type In = In;
    type Out = Out;
    type RecvStream = MappedRecvStream<C::RecvStream, In>;
    type SendSink = MappedSendSink<C::SendSink, Out, C::Out>;
}

impl<In, Out, C> Connection for MappedConnection<In, Out, C>
where
    C: Connection,
    In: RpcMessage,
    Out: RpcMessage,
    In: TryFrom<C::In>,
    C::Out: From<Out>,
{
    fn open(
        &self,
    ) -> impl std::future::Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>
           + Send {
        let inner = self.inner.open();
        async move {
            let (send, recv) = inner.await?;
            Ok((MappedSendSink::new(send), MappedRecvStream::new(recv)))
        }
    }
}

/// A combinator that maps a stream of incoming messages to a different type
#[pin_project]
pub struct MappedRecvStream<S, In> {
    inner: S,
    _phantom: std::marker::PhantomData<In>,
}

impl<S, In> MappedRecvStream<S, In> {
    /// Create a new mapped receive stream
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

/// Error mapping an incoming message to the inner type
#[derive(Debug)]
pub enum ErrorOrMapError<E> {
    /// Error from the inner stream
    Inner(E),
    /// Conversion error
    Conversion,
}

impl<E: Debug + Display> std::error::Error for ErrorOrMapError<E> {}

impl<E: Display> Display for ErrorOrMapError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErrorOrMapError::Inner(e) => write!(f, "Inner error: {}", e),
            ErrorOrMapError::Conversion => write!(f, "Conversion error"),
        }
    }
}

impl<S, In0, In, E> Stream for MappedRecvStream<S, In>
where
    S: Stream<Item = Result<In0, E>> + Unpin,
    In: TryFrom<In0>,
    E: RpcError,
{
    type Item = Result<In, ErrorOrMapError<E>>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        match self.project().inner.poll_next(cx) {
            Poll::Ready(Some(Ok(item))) => {
                let item = item.try_into().map_err(|_| ErrorOrMapError::Conversion);
                Poll::Ready(Some(item))
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(ErrorOrMapError::Inner(e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// A sink that maps outgoing messages to a different type
///
/// The conversion to the underlying message type always succeeds, so this
/// is relatively simple.
#[pin_project]
pub struct MappedSendSink<S, Out, OutS> {
    inner: S,
    _phantom: std::marker::PhantomData<(Out, OutS)>,
}

impl<S, Out, Out0> MappedSendSink<S, Out, Out0> {
    /// Create a new mapped send sink
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<S, Out, Out0> futures_sink::Sink<Out> for MappedSendSink<S, Out, Out0>
where
    S: futures_sink::Sink<Out0> + Unpin,
    Out: Into<Out0>,
{
    type Error = S::Error;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready_unpin(cx)
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        self.project().inner.start_send_unpin(item.into())
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close_unpin(cx)
    }
}

/// Extension trait for mapping connections
pub trait ConnectionMapExt<In, Out>: Connection<In = In, Out = Out> {
    /// Map the input and output types of this connection
    fn map<In1, Out1>(self) -> MappedConnection<In1, Out1, Self>
    where
        In1: TryFrom<In>,
        Out: From<Out1>,
    {
        MappedConnection::new(self)
    }

    /// Map this connection to a service
    fn map_to_service<S: Service>(self) -> MappedConnection<S::Res, S::Req, Self>
    where
        S::Res: TryFrom<In>,
        Out: From<S::Req>,
    {
        MappedConnection::new(self)
    }
}

impl<C: Connection> ConnectionMapExt<C::In, C::Out> for C {}

/// Connection types for a mapped connection
pub struct MappedConnectionTypes<In, Out, C>(PhantomData<(In, Out, C)>);

impl<In, Out, C> Debug for MappedConnectionTypes<In, Out, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedConnectionTypes").finish()
    }
}

impl<In, Out, C> Clone for MappedConnectionTypes<In, Out, C> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}

impl<In, Out, C> ConnectionErrors for MappedConnectionTypes<In, Out, C>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ConnectionErrors,
{
    type RecvError = ErrorOrMapError<C::RecvError>;
    type SendError = C::SendError;
    type OpenError = C::OpenError;
    type AcceptError = C::AcceptError;
}

impl<In, Out, C> ConnectionCommon for MappedConnectionTypes<In, Out, C>
where
    C: ConnectionCommon,
    In: RpcMessage,
    Out: RpcMessage,
    In: TryFrom<C::In>,
    C::Out: From<Out>,
{
    type In = In;
    type Out = Out;
    type RecvStream = MappedRecvStream<C::RecvStream, In>;
    type SendSink = MappedSendSink<C::SendSink, Out, C::Out>;
}

impl<S, C> RpcChannel<S, C>
where
    S: Service,
    C: ConnectionCommon<In = S::Req, Out = S::Res>,
{
    /// Map the input and output types of this connection
    pub fn map2<S1>(self) -> RpcChannel<S1, MappedConnectionTypes<S1::Req, S1::Res, C>>
    where
        S1: Service,
        S1::Req: TryFrom<S::Req>,
        S::Res: From<S1::Res>,
    {
        let send = MappedSendSink::<C::SendSink, S1::Res, S::Res>::new(self.send);
        let recv = MappedRecvStream::<C::RecvStream, S1::Req>::new(self.recv);
        let t: RpcChannel<S1, MappedConnectionTypes<S1::Req, S1::Res, C>> = RpcChannel {
            send,
            recv,
            p: PhantomData,
        };
        t
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        client::BoxedServiceConnection,
        server::{BoxedServiceChannel, RpcChannel},
        transport::{boxed::BoxableConnection, flume::FlumeConnection, ServerEndpoint},
        RpcClient, RpcServer, ServiceConnection, ServiceEndpoint,
    };
    use serde::{Deserialize, Serialize};
    use testresult::TestResult;

    use super::*;

    #[derive(Debug, Clone, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Request {
        A(u64),
        B(String),
    }

    #[derive(Debug, Clone, Serialize, Deserialize, derive_more::From, derive_more::TryInto)]
    enum Response {
        A(u64),
        B(String),
    }

    #[derive(Debug, Clone)]
    struct FullService;

    impl crate::Service for FullService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Debug, Clone)]
    struct SubService;

    impl crate::Service for SubService {
        type Req = String;
        type Res = String;
    }

    #[tokio::test]
    async fn smoke() -> TestResult<()> {
        async fn handle_sub_request(
            req: String,
            chan: RpcChannel<SubService, BoxedServiceChannel<SubService>>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        let (s, c): (_, FlumeConnection<Response, Request>) =
            crate::transport::flume::connection::<Request, Response>(32);
        let _x = c.clone().map::<String, String>();

        let _y = c.clone().map_to_service::<SubService>();
        let s = RpcServer::<FullService, _>::new(s);
        return Ok(());
        while let Ok(accepting) = s.accept().await {
            let (msg, chan) = accepting.read_first().await?;
            match msg {
                Request::A(x) => todo!(),
                Request::B(x) => handle_sub_request(x, chan.map::<SubService>().boxed()).await?,
            }
        }
        Ok(())
    }
}
