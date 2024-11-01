//! Transport with mapped input and output types.
use std::{
    fmt::{Debug, Display},
    iter::Rev,
    marker::PhantomData,
    task::{Context, Poll},
};

use futures_lite::{Stream, StreamExt};
use futures_util::SinkExt;
use pin_project::pin_project;

use crate::{server::RpcChannel, RpcError, RpcMessage, Service, ServiceEndpoint};

use super::{Connection, ConnectionCommon, ConnectionErrors};

/// A connection that maps input and output types
#[derive(Debug)]
pub struct MappedConnection<In, Out, In0, Out0, T> {
    inner: T,
    _phantom: std::marker::PhantomData<(In, Out, In0, Out0)>,
}

impl<In, Out, InT, OutT, T> MappedConnection<In, Out, InT, OutT, T>
where
    T: Connection<InT, OutT>,
    In: TryFrom<InT>,
    OutT: From<Out>,
{
    /// Create a new mapped connection
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<In, Out, InT, OutT, T> Clone for MappedConnection<In, Out, InT, OutT, T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<In, Out, InT, OutT, T> ConnectionErrors for MappedConnection<In, Out, InT, OutT, T>
where
    In: RpcMessage,
    Out: RpcMessage,
    InT: RpcMessage,
    OutT: RpcMessage,
    T: ConnectionErrors,
{
    type OpenError = T::OpenError;
    type RecvError = ErrorOrMapError<T::RecvError>;
    type SendError = T::SendError;
}

impl<In, Out, InT, OutT, T> ConnectionCommon<In, Out> for MappedConnection<In, Out, InT, OutT, T>
where
    T: ConnectionCommon<InT, OutT>,
    In: RpcMessage,
    Out: RpcMessage,
    InT: RpcMessage,
    OutT: RpcMessage,
    In: TryFrom<InT>,
    OutT: From<Out>,
{
    type RecvStream = MappedRecvStream<T::RecvStream, In>;
    type SendSink = MappedSendSink<T::SendSink, Out, OutT>;
}

impl<In, Out, InT, OutT, T> Connection<In, Out> for MappedConnection<In, Out, InT, OutT, T>
where
    T: Connection<InT, OutT>,
    In: RpcMessage,
    Out: RpcMessage,
    InT: RpcMessage,
    OutT: RpcMessage,
    In: TryFrom<InT>,
    OutT: From<Out>,
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
pub trait ConnectionMapExt<In, Out>: Connection<In, Out> {
    /// Map the input and output types of this connection
    fn map<In1, Out1>(self) -> MappedConnection<In1, Out1, In, Out, Self>
    where
        In1: TryFrom<In>,
        Out: From<Out1>,
    {
        MappedConnection::new(self)
    }

    /// Map this connection to a service
    fn map_to_service<S: Service>(self) -> MappedConnection<S::Res, S::Req, In, Out, Self>
    where
        S::Res: TryFrom<In>,
        Out: From<S::Req>,
    {
        MappedConnection::new(self)
    }
}

impl<T: Connection<In, Out>, In, Out> ConnectionMapExt<In, Out> for T {}

struct MappedStreamTypes<Send, Recv, OE> {
    p: PhantomData<(Send, Recv, OE)>,
}

impl<Send, Recv, OE> std::fmt::Debug for MappedStreamTypes<Send, Recv, OE> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedStreamTypes").finish()
    }
}

impl<Send, Recv, OE> Clone for MappedStreamTypes<Send, Recv, OE> {
    fn clone(&self) -> Self {
        Self { p: PhantomData }
    }
}

struct MappedCC<In, Out, C>(PhantomData<(In, Out, C)>);

impl<In, Out, C> Debug for MappedCC<In, Out, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MappedCC").finish()
    }
}

impl<In, Out, C> Clone for MappedCC<In, Out, C> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}

impl<In, Out, C> ConnectionErrors for MappedCC<In, Out, C>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ConnectionErrors,
{
    type OpenError = C::OpenError;
    type RecvError = ErrorOrMapError<C::RecvError>;
    type SendError = C::SendError;
}

impl<In, Out, C> ConnectionCommon<In, Out> for MappedCC<In, Out, C>
where
    C: ConnectionCommon<In, Out>,
    In: RpcMessage,
    Out: RpcMessage,
{
    type RecvStream = MappedRecvStream<C::RecvStream, In>;
    type SendSink = MappedSendSink<C::SendSink, Out, Out>;
}

struct StreamPair<Send, Recv> {
    pub send: Send,
    pub recv: Recv,
}

impl<Send, Recv> StreamPair<Send, Recv> {
    pub fn new(send: Send, recv: Recv) -> Self {
        Self { send, recv }
    }

    pub fn map<In, Out, In0, Out0, E>(
        self,
    ) -> StreamPair<MappedSendSink<Send, Out, Out0>, MappedRecvStream<Recv, In>>
    where
        Send: futures_sink::Sink<Out0> + Unpin,
        Recv: Stream<Item = Result<In0, E>> + Unpin,
        Out0: From<Out>,
        In: TryFrom<In0>,
    {
        StreamPair::new(
            MappedSendSink::new(self.send),
            MappedRecvStream::new(self.recv),
        )
    }
}

#[cfg(test)]
mod tests {

    use crate::{
        server::RpcChannel,
        transport::{flume::FlumeConnection, ServerEndpoint},
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
            chan: RpcChannel<SubService, impl Connection<String, String>>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        let (s, c): (_, FlumeConnection<Response, Request>) =
            crate::transport::flume::connection::<Request, Response>(32);
        let _x = c.clone().map::<String, String>();
        let _y = c.clone().map_to_service::<SubService>();
        let _z = RpcClient::<SubService, _>::new(c.map_to_service::<SubService>());
        let s = RpcServer::<FullService, _>::new(s);

        while let Ok(accepting) = s.accept().await {
            let (msg, chan) = accepting.read_first().await?;
            match msg {
                Request::A(x) => todo!(),
                Request::B(x) => todo!(), // handle_sub_request(x, chan).await?,
            }
        }
        Ok(())
    }
}
