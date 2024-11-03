//! Boxed transport with concrete types

use std::{
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll},
};

use futures_lite::FutureExt;
use futures_sink::Sink;
use futures_util::{future::BoxFuture, SinkExt, Stream, StreamExt, TryStreamExt};
use pin_project::pin_project;
use std::future::Future;

use crate::RpcMessage;

use super::{ConnectionCommon, ConnectionErrors};
type BoxedFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + Sync + 'a>>;

enum SendSinkInner<T: RpcMessage> {
    #[cfg(feature = "flume-transport")]
    Direct(::flume::r#async::SendSink<'static, T>),
    Boxed(Pin<Box<dyn Sink<T, Error = anyhow::Error> + Send + Sync + 'static>>),
}

/// A sink that can be used to send messages to the remote end of a channel.
///
/// For local channels, this is a thin wrapper around a flume send sink.
/// For network channels, this contains a boxed sink, since it is reasonable
/// to assume that in that case the additional overhead of boxing is negligible.
#[pin_project]
pub struct SendSink<T: RpcMessage>(SendSinkInner<T>);

impl<T: RpcMessage> SendSink<T> {
    /// Create a new send sink from a boxed sink
    pub fn boxed(sink: impl Sink<T, Error = anyhow::Error> + Send + Sync + 'static) -> Self {
        Self(SendSinkInner::Boxed(Box::pin(sink)))
    }

    /// Create a new send sink from a direct flume send sink
    #[cfg(feature = "flume-transport")]
    pub(crate) fn direct(sink: ::flume::r#async::SendSink<'static, T>) -> Self {
        Self(SendSinkInner::Direct(sink))
    }
}

impl<T: RpcMessage> Sink<T> for SendSink<T> {
    type Error = anyhow::Error;

    fn poll_ready(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            SendSinkInner::Direct(sink) => sink.poll_ready_unpin(cx).map_err(anyhow::Error::from),
            SendSinkInner::Boxed(sink) => sink.poll_ready_unpin(cx).map_err(anyhow::Error::from),
        }
    }

    fn start_send(self: std::pin::Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            SendSinkInner::Direct(sink) => sink.start_send_unpin(item).map_err(anyhow::Error::from),
            SendSinkInner::Boxed(sink) => sink.start_send_unpin(item).map_err(anyhow::Error::from),
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            SendSinkInner::Direct(sink) => sink.poll_flush_unpin(cx).map_err(anyhow::Error::from),
            SendSinkInner::Boxed(sink) => sink.poll_flush_unpin(cx).map_err(anyhow::Error::from),
        }
    }

    fn poll_close(
        self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            SendSinkInner::Direct(sink) => sink.poll_close_unpin(cx).map_err(anyhow::Error::from),
            SendSinkInner::Boxed(sink) => sink.poll_close_unpin(cx).map_err(anyhow::Error::from),
        }
    }
}

enum RecvStreamInner<T: RpcMessage> {
    #[cfg(feature = "flume-transport")]
    Direct(::flume::r#async::RecvStream<'static, T>),
    Boxed(Pin<Box<dyn Stream<Item = Result<T, anyhow::Error>> + Send + Sync + 'static>>),
}

/// A stream that can be used to receive messages from the remote end of a channel.
///
/// For local channels, this is a thin wrapper around a flume receive stream.
/// For network channels, this contains a boxed stream, since it is reasonable
#[pin_project]
pub struct RecvStream<T: RpcMessage>(RecvStreamInner<T>);

impl<T: RpcMessage> RecvStream<T> {
    /// Create a new receive stream from a boxed stream
    pub fn boxed(
        stream: impl Stream<Item = Result<T, anyhow::Error>> + Send + Sync + 'static,
    ) -> Self {
        Self(RecvStreamInner::Boxed(Box::pin(stream)))
    }

    /// Create a new receive stream from a direct flume receive stream
    #[cfg(feature = "flume-transport")]
    pub(crate) fn direct(stream: ::flume::r#async::RecvStream<'static, T>) -> Self {
        Self(RecvStreamInner::Direct(stream))
    }
}

impl<T: RpcMessage> Stream for RecvStream<T> {
    type Item = Result<T, anyhow::Error>;

    fn poll_next(self: std::pin::Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            RecvStreamInner::Direct(stream) => match stream.poll_next_unpin(cx) {
                Poll::Ready(Some(item)) => Poll::Ready(Some(Ok(item))),
                Poll::Ready(None) => Poll::Ready(None),
                Poll::Pending => Poll::Pending,
            },
            RecvStreamInner::Boxed(stream) => stream.poll_next_unpin(cx),
        }
    }
}

enum OpenFutureInner<'a, In: RpcMessage, Out: RpcMessage> {
    /// A direct future (todo)
    #[cfg(feature = "flume-transport")]
    Direct(super::flume::OpenFuture<In, Out>),
    /// A boxed future
    Boxed(BoxFuture<'a, anyhow::Result<(SendSink<Out>, RecvStream<In>)>>),
}

/// A concrete future for opening a channel
#[pin_project]
pub struct OpenFuture<'a, In: RpcMessage, Out: RpcMessage>(OpenFutureInner<'a, In, Out>);

impl<'a, In: RpcMessage, Out: RpcMessage> OpenFuture<'a, In, Out> {
    #[cfg(feature = "flume-transport")]
    fn direct(f: super::flume::OpenFuture<In, Out>) -> Self {
        Self(OpenFutureInner::Direct(f))
    }

    /// Create a new boxed future
    pub fn boxed(
        f: impl Future<Output = anyhow::Result<(SendSink<Out>, RecvStream<In>)>> + Send + 'a,
    ) -> Self {
        Self(OpenFutureInner::Boxed(Box::pin(f)))
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenFuture<'a, In, Out> {
    type Output = anyhow::Result<(SendSink<Out>, RecvStream<In>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            OpenFutureInner::Direct(f) => f
                .poll(cx)
                .map_ok(|(send, recv)| (SendSink::direct(send.0), RecvStream::direct(recv.0)))
                .map_err(|e| e.into()),
            OpenFutureInner::Boxed(f) => f.poll(cx),
        }
    }
}

enum AcceptFutureInner<'a, In: RpcMessage, Out: RpcMessage> {
    /// A direct future
    #[cfg(feature = "flume-transport")]
    Direct(super::flume::AcceptFuture<In, Out>),
    /// A boxed future
    Boxed(BoxedFuture<'a, anyhow::Result<(SendSink<Out>, RecvStream<In>)>>),
}

/// Concrete accept future
#[pin_project]
pub struct AcceptFuture<'a, In: RpcMessage, Out: RpcMessage>(AcceptFutureInner<'a, In, Out>);

impl<'a, In: RpcMessage, Out: RpcMessage> AcceptFuture<'a, In, Out> {
    #[cfg(feature = "flume-transport")]
    fn direct(f: super::flume::AcceptFuture<In, Out>) -> Self {
        Self(AcceptFutureInner::Direct(f))
    }

    /// Create a new boxed future
    pub fn boxed(
        f: impl Future<Output = anyhow::Result<(SendSink<Out>, RecvStream<In>)>> + Send + Sync + 'a,
    ) -> Self {
        Self(AcceptFutureInner::Boxed(Box::pin(f)))
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for AcceptFuture<'a, In, Out> {
    type Output = anyhow::Result<(SendSink<Out>, RecvStream<In>)>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        match self.project().0 {
            #[cfg(feature = "flume-transport")]
            AcceptFutureInner::Direct(f) => f
                .poll(cx)
                .map_ok(|(send, recv)| (SendSink::direct(send.0), RecvStream::direct(recv.0)))
                .map_err(|e| e.into()),
            AcceptFutureInner::Boxed(f) => f.poll(cx),
        }
    }
}

/// A boxable connection
pub trait BoxableConnection<In: RpcMessage, Out: RpcMessage>:
    Debug + Send + Sync + 'static
{
    /// Clone the connection and box it
    fn clone_box(&self) -> Box<dyn BoxableConnection<In, Out>>;

    /// Open a channel to the remote che
    fn open_boxed(&self) -> OpenFuture<In, Out>;
}

/// A boxed connection
#[derive(Debug)]
pub struct Connection<In, Out>(Box<dyn BoxableConnection<In, Out>>);

impl<In: RpcMessage, Out: RpcMessage> Connection<In, Out> {
    /// Wrap a boxable server endpoint into a box, transforming all the types to concrete types
    pub fn new(x: impl BoxableConnection<In, Out>) -> Self {
        Self(Box::new(x))
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for Connection<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon for Connection<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = RecvStream<In>;
    type SendSink = SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for Connection<In, Out> {
    type SendError = anyhow::Error;
    type RecvError = anyhow::Error;
    type OpenError = anyhow::Error;
    type AcceptError = anyhow::Error;
}

impl<In: RpcMessage, Out: RpcMessage> super::Connection for Connection<In, Out> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        self.0.open_boxed().await
    }
}

/// A boxable server endpoint
pub trait BoxableServerEndpoint<In: RpcMessage, Out: RpcMessage>:
    Debug + Send + Sync + 'static
{
    /// Clone the server endpoint and box it
    fn clone_box(&self) -> Box<dyn BoxableServerEndpoint<In, Out>>;

    /// Accept a channel from a remote client
    fn accept_bi_boxed(&self) -> AcceptFuture<In, Out>;

    /// Get the local address
    fn local_addr(&self) -> &[super::LocalAddr];
}

/// A boxed server endpoint
#[derive(Debug)]
pub struct ServerEndpoint<In: RpcMessage, Out: RpcMessage>(Box<dyn BoxableServerEndpoint<In, Out>>);

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint<In, Out> {
    /// Wrap a boxable server endpoint into a box, transforming all the types to concrete types
    pub fn new(x: impl BoxableServerEndpoint<In, Out>) -> Self {
        Self(Box::new(x))
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon for ServerEndpoint<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = RecvStream<In>;
    type SendSink = SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for ServerEndpoint<In, Out> {
    type SendError = anyhow::Error;
    type RecvError = anyhow::Error;
    type OpenError = anyhow::Error;
    type AcceptError = anyhow::Error;
}

impl<In: RpcMessage, Out: RpcMessage> super::ServerEndpoint for ServerEndpoint<In, Out> {
    fn accept(
        &self,
    ) -> impl Future<Output = Result<(Self::SendSink, Self::RecvStream), Self::AcceptError>> + Send
    {
        self.0.accept_bi_boxed()
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        self.0.local_addr()
    }
}
impl<In: RpcMessage, Out: RpcMessage> BoxableConnection<In, Out> for Connection<In, Out> {
    fn clone_box(&self) -> Box<dyn BoxableConnection<In, Out>> {
        Box::new(self.clone())
    }

    fn open_boxed(&self) -> OpenFuture<In, Out> {
        OpenFuture::boxed(crate::transport::Connection::open(self))
    }
}

#[cfg(feature = "quinn-transport")]
impl<In: RpcMessage, Out: RpcMessage> BoxableConnection<In, Out>
    for super::quinn::QuinnConnection<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableConnection<In, Out>> {
        Box::new(self.clone())
    }

    fn open_boxed(&self) -> OpenFuture<In, Out> {
        let f = Box::pin(async move {
            let (send, recv) = super::Connection::open(self).await?;
            // map the error types to anyhow
            let send = send.sink_map_err(anyhow::Error::from);
            let recv = recv.map_err(anyhow::Error::from);
            // return the boxed streams
            anyhow::Ok((SendSink::boxed(send), RecvStream::boxed(recv)))
        });
        OpenFuture::boxed(f)
    }
}

#[cfg(feature = "quinn-transport")]
impl<In: RpcMessage, Out: RpcMessage> BoxableServerEndpoint<In, Out>
    for super::quinn::QuinnServerEndpoint<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableServerEndpoint<In, Out>> {
        Box::new(self.clone())
    }

    fn accept_bi_boxed(&self) -> AcceptFuture<In, Out> {
        let f = async move {
            let (send, recv) = super::ServerEndpoint::accept(self).await?;
            let send = send.sink_map_err(anyhow::Error::from);
            let recv = recv.map_err(anyhow::Error::from);
            anyhow::Ok((SendSink::boxed(send), RecvStream::boxed(recv)))
        };
        AcceptFuture::boxed(f)
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        super::ServerEndpoint::local_addr(self)
    }
}

#[cfg(feature = "flume-transport")]
impl<In: RpcMessage, Out: RpcMessage> BoxableConnection<In, Out>
    for super::flume::FlumeConnection<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableConnection<In, Out>> {
        Box::new(self.clone())
    }

    fn open_boxed(&self) -> OpenFuture<In, Out> {
        OpenFuture::direct(super::Connection::open(self))
    }
}

#[cfg(feature = "flume-transport")]
impl<In: RpcMessage, Out: RpcMessage> BoxableServerEndpoint<In, Out>
    for super::flume::FlumeServerEndpoint<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableServerEndpoint<In, Out>> {
        Box::new(self.clone())
    }

    fn accept_bi_boxed(&self) -> AcceptFuture<In, Out> {
        AcceptFuture::direct(super::ServerEndpoint::accept(self))
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        super::ServerEndpoint::local_addr(self)
    }
}

impl<In, Out, C> BoxableConnection<In, Out> for super::mapped::MappedConnection<In, Out, C>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: super::Connection,
    C::Out: From<Out>,
    In: TryFrom<C::In>,
    C::SendError: Into<anyhow::Error>,
    C::RecvError: Into<anyhow::Error>,
    C::OpenError: Into<anyhow::Error>,
{
    fn clone_box(&self) -> Box<dyn BoxableConnection<In, Out>> {
        Box::new(self.clone())
    }

    fn open_boxed(&self) -> OpenFuture<In, Out> {
        let f = Box::pin(async move {
            let (send, recv) = super::Connection::open(self).await.map_err(|e| e.into())?;
            // map the error types to anyhow
            let send = send.sink_map_err(|e| e.into());
            let recv = recv.map_err(|e| e.into());
            // return the boxed streams
            anyhow::Ok((SendSink::boxed(send), RecvStream::boxed(recv)))
        });
        OpenFuture::boxed(f)
    }
}

#[cfg(test)]
mod tests {
    use crate::Service;

    #[derive(Debug, Clone)]
    struct FooService;

    impl Service for FooService {
        type Req = u64;
        type Res = u64;
    }

    #[cfg(feature = "flume-transport")]
    #[tokio::test]
    async fn box_smoke() {
        use futures_lite::StreamExt;
        use futures_util::SinkExt;

        use crate::transport::{Connection, ServerEndpoint};

        let (server, client) = crate::transport::flume::connection(1);
        let server = super::ServerEndpoint::new(server);
        let client = super::Connection::new(client);
        // spawn echo server
        tokio::spawn(async move {
            while let Ok((mut send, mut recv)) = server.accept().await {
                if let Some(Ok(msg)) = recv.next().await {
                    send.send(msg).await.ok();
                }
            }
            anyhow::Ok(())
        });
        if let Ok((mut send, mut recv)) = client.open().await {
            send.send(1).await.ok();
            let res = recv.next().await;
            println!("{:?}", res);
        }
    }
}
