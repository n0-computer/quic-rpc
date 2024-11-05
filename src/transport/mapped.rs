//! Transport with mapped input and output types.
use std::{
    fmt::{Debug, Display},
    marker::PhantomData,
    task::{Context, Poll},
};

use futures_lite::{Stream, StreamExt};
use futures_util::SinkExt;
use pin_project::pin_project;

use crate::{RpcError, RpcMessage};

use super::{ConnectionErrors, Connector, StreamTypes};

/// A connection that maps input and output types
#[derive(Debug)]
pub struct MappedConnection<In, Out, C> {
    inner: C,
    _p: std::marker::PhantomData<(In, Out)>,
}

impl<In, Out, C> MappedConnection<In, Out, C>
where
    C: Connector,
    In: TryFrom<C::In>,
    C::Out: From<Out>,
{
    /// Create a new mapped connection
    pub fn new(inner: C) -> Self {
        Self {
            inner,
            _p: std::marker::PhantomData,
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
            _p: std::marker::PhantomData,
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

impl<In, Out, C> StreamTypes for MappedConnection<In, Out, C>
where
    C: StreamTypes,
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

impl<In, Out, C> Connector for MappedConnection<In, Out, C>
where
    C: Connector,
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
    _p: std::marker::PhantomData<In>,
}

impl<S, In> MappedRecvStream<S, In> {
    /// Create a new mapped receive stream
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _p: std::marker::PhantomData,
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
    _p: std::marker::PhantomData<(Out, OutS)>,
}

impl<S, Out, Out0> MappedSendSink<S, Out, Out0> {
    /// Create a new mapped send sink
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            _p: std::marker::PhantomData,
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

impl<In, Out, C> StreamTypes for MappedConnectionTypes<In, Out, C>
where
    C: StreamTypes,
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

#[cfg(test)]
#[cfg(feature = "flume-transport")]
mod tests {

    use crate::{
        server::{BoxedServiceChannel, RpcChannel},
        transport::boxed::BoxableListener,
        RpcClient, RpcServer,
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
    #[ignore]
    async fn smoke() -> TestResult<()> {
        async fn handle_sub_request(
            _req: String,
            _chan: RpcChannel<SubService, BoxedServiceChannel<SubService>>,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        // create a server endpoint / connection pair. Type will be inferred
        let (s, c) = crate::transport::flume::connection(32);
        // wrap the server in a RpcServer, this is where the service type is specified
        let server = RpcServer::<FullService, _>::new(s.clone());
        // when using a boxed transport, we can omit the transport type and use the default
        let _server_boxed: RpcServer<FullService> = RpcServer::<FullService>::new(s.boxed());
        // create a client in a RpcClient, this is where the service type is specified
        let client = RpcClient::<FullService, _>::new(c);
        // when using a boxed transport, we can omit the transport type and use the default
        let _boxed_client = client.clone().boxed();
        // map the client to a sub-service
        let _sub_client: RpcClient<SubService, _> = client.clone().map::<SubService>();
        // when using a boxed transport, we can omit the transport type and use the default
        let _sub_client_boxed: RpcClient<SubService> = client.clone().map::<SubService>().boxed();
        // we can not map the service to a sub-service, since we need the first message to determine which sub-service to use
        while let Ok(accepting) = server.accept().await {
            let (msg, chan) = accepting.read_first().await?;
            match msg {
                Request::A(_x) => todo!(),
                Request::B(x) => {
                    // but we can map the channel to the sub-service, once we know which one to use
                    handle_sub_request(x, chan.map::<SubService>().boxed()).await?
                }
            }
        }
        Ok(())
    }
}
