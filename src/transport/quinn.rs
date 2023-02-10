//! QUIC channel implementation based on quinn
use crate::client2::{TypedConnection, ConnectionErrors};
use crate::{LocalAddr, RpcMessage};
use futures::channel::oneshot;
use futures::future::BoxFuture;
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{self, ready, Poll};
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

use super::util::FramedBincode;

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

#[derive(Debug)]
struct ServerChannelInner {
    endpoint: Option<quinn::Endpoint>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_addr: [LocalAddr; 1],
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ServerChannelInner {
    fn drop(&mut self) {
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close(0u32.into(), b"server channel dropped");
        }
        if let Some(task) = self.task.take() {
            task.abort()
        }
    }
}

/// A server channel using a quinn connection
#[derive(Debug)]
pub struct QuinnServerChannel<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ServerChannelInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnServerChannel<In, Out> {
    /// handles RPC requests from a connection
    ///
    /// to cleanly shutdown the handler, drop the receiver side of the sender.
    async fn connection_handler(connection: quinn::Connection, sender: flume::Sender<SocketInner>) {
        loop {
            tracing::debug!("Awaiting incoming bidi substream on existing connection...");
            let bidi_stream = match connection.accept_bi().await {
                Ok(bidi_stream) => bidi_stream,
                Err(quinn::ConnectionError::ApplicationClosed(e)) => {
                    tracing::debug!("Peer closed the connection {:?}", e);
                    break;
                }
                Err(e) => {
                    tracing::debug!("Error opening stream: {}", e);
                    break;
                }
            };
            tracing::debug!("Sending substream to be handled... {}", bidi_stream.0.id());
            if sender.send_async(bidi_stream).await.is_err() {
                tracing::debug!("Receiver dropped");
                break;
            }
        }
    }

    async fn endpoint_handler(endpoint: quinn::Endpoint, sender: flume::Sender<SocketInner>) {
        loop {
            tracing::info!("Waiting for incoming connection...");
            let connecting = match endpoint.accept().await {
                Some(connecting) => connecting,
                None => break,
            };
            tracing::info!("Awaiting connection from connect...");
            let conection = match connecting.await {
                Ok(conection) => conection,
                Err(e) => {
                    tracing::warn!("Error accepting connection: {}", e);
                    continue;
                }
            };
            tracing::info!(
                "Connection established from {:?}",
                conection.remote_address()
            );
            tracing::info!("Spawning connection handler...");
            tokio::spawn(Self::connection_handler(conection, sender.clone()));
        }
    }

    /// Create a new server channel, given a quinn endpoint.
    ///
    /// The endpoint must be a server endpoint.
    ///
    /// The server channel will take care of listening on the endpoint and spawning
    /// handlers for new connections.
    pub fn new(endpoint: quinn::Endpoint) -> io::Result<Self> {
        let local_addr = endpoint.local_addr()?;
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::endpoint_handler(endpoint.clone(), sender));
        Ok(Self {
            inner: Arc::new(ServerChannelInner {
                endpoint: Some(endpoint),
                task: Some(task),
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        })
    }

    /// Create a new server channel, given just a source of incoming connections
    ///
    /// This is useful if you want to manage the quinn endpoint yourself,
    /// use multiple endpoints, or use an endpoint for multiple protocols.
    pub fn handle_connections(
        incoming: flume::Receiver<quinn::Connection>,
        local_addr: SocketAddr,
    ) -> Self {
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(async move {
            // just grab all connections and spawn a handler for each one
            while let Ok(connection) = incoming.recv_async().await {
                tokio::spawn(Self::connection_handler(connection, sender.clone()));
            }
        });
        Self {
            inner: Arc::new(ServerChannelInner {
                endpoint: None,
                task: Some(task),
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        }
    }

    /// Create a new server channel, given just a source of incoming substreams
    ///
    /// This is useful if you want to manage the quinn endpoint yourself,
    /// use multiple endpoints, or use an endpoint for multiple protocols.
    pub fn handle_substreams(
        receiver: flume::Receiver<SocketInner>,
        local_addr: SocketAddr,
    ) -> Self {
        Self {
            inner: Arc::new(ServerChannelInner {
                endpoint: None,
                task: None,
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnServerChannel<In, Out> {
    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> TypedConnection<In, Out> for QuinnServerChannel<In, Out> {
    type Channel = FramedBincode<QuinnSocket, In, Out>;

    type NextFut<'a> = BoxFuture<'a, result::Result<FramedBincode<QuinnSocket, In, Out>, quinn::ConnectionError>>;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let (send, recv) = self.inner.receiver.recv_async().await.map_err(|_| quinn::ConnectionError::LocallyClosed)?;
            let chan = QuinnSocket { send, recv };
            let wrapped = FramedBincode::new(chan, 1024 * 1024);
            Ok(wrapped)
        }.boxed()
    }
}

type SocketInner = (quinn::SendStream, quinn::RecvStream);

#[derive(Debug)]
struct ClientChannelInner {
    /// The quinn endpoint, we just keep a clone of this for information
    endpoint: Option<quinn::Endpoint>,
    /// The task that handles creating new connections
    task: Option<tokio::task::JoinHandle<()>>,
    /// The channel to receive new connections
    sender: flume::Sender<oneshot::Sender<SocketInner>>,
}

impl Drop for ClientChannelInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping client channel");
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close(0u32.into(), b"client channel dropped");
        }
        // this should not be necessary, since the task would terminate when the receiver is dropped.
        // but just to be on the safe side.
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A client channel using a quinn connection
pub struct QuinnClientChannel<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ClientChannelInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnClientChannel<In, Out> {
    /// Client connection handler.
    ///
    /// It will run until the send side of the channel is dropped.
    /// All other errors are logged and handled internally.
    /// It will try to keep a connection open at all times.
    async fn connection_handler_inner(
        endpoint: quinn::Endpoint,
        addr: SocketAddr,
        name: String,
        requests: flume::Receiver<oneshot::Sender<SocketInner>>,
    ) -> result::Result<(), flume::RecvError> {
        'outer: loop {
            tracing::info!("Connecting to {} as {}", addr, name);
            let connecting = match endpoint.connect(addr, &name) {
                Ok(connecting) => connecting,
                Err(e) => {
                    tracing::warn!("error calling connect: {}", e);
                    // try again. Maybe delay?
                    continue;
                }
            };
            let connection = match connecting.await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::warn!("error awaiting connect: {}", e);
                    // try again. Maybe delay?
                    continue;
                }
            };
            loop {
                tracing::debug!("Awaiting request for new bidi substream...");
                let request = requests.recv_async().await?;
                tracing::debug!("Got request for new bidi substream");
                match connection.open_bi().await {
                    Ok(pair) => {
                        tracing::debug!("Bidi substream opened");
                        if request.send(pair).is_err() {
                            tracing::debug!("requester dropped");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("error opening bidi substream: {}", e);
                        tracing::warn!("recreating connection");
                        // try again. Maybe delay?
                        continue 'outer;
                    }
                }
            }
        }
    }

    async fn connection_handler(
        endpoint: quinn::Endpoint,
        addr: SocketAddr,
        name: String,
        requests: flume::Receiver<oneshot::Sender<SocketInner>>,
    ) {
        if let Err(res) = Self::connection_handler_inner(endpoint, addr, name, requests).await {
            tracing::info!("Connection handler finished: {:?}", res);
        } else {
            unreachable!()
        }
    }

    /// Create a new channel
    pub fn new(endpoint: quinn::Endpoint, addr: SocketAddr, name: String) -> Self {
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::connection_handler(
            endpoint.clone(),
            addr,
            name,
            receiver,
        ));
        Self {
            inner: Arc::new(ClientChannelInner {
                endpoint: Some(endpoint),
                task: Some(task),
                sender,
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for QuinnClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnClientChannel<In, Out> {
    
    type SendError = io::Error;
    
    type RecvError = io::Error;

    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> TypedConnection<In, Out> for QuinnClientChannel<In, Out> {
    type Channel = FramedBincode<QuinnSocket, In, Out>;

    type NextFut<'a> = BoxFuture<'a, result::Result<FramedBincode<QuinnSocket, In, Out>, quinn::ConnectionError>>;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let (sender, receiver) = oneshot::channel();
            self.inner
                .sender
                .send_async(sender)
                .await
                .map_err(|_| quinn::ConnectionError::LocallyClosed)?;
            receiver
                .await
                .map(|(send, recv)| FramedBincode::new(QuinnSocket { send, recv }, 1024 * 1024))
                .map_err(|_| quinn::ConnectionError::LocallyClosed)
        }
        .boxed()
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct SendSink<Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedWrite<::quinn::SendStream, LengthDelimitedCodec>,
        Out,
        SymmetricalBincode<Out>,
    >,
);

impl<Out: Serialize> Sink<Out> for SendSink<Out> {
    type Error = io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        self.project().0.start_send_unpin(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

/// A stream that wraps a quinn RecvStream with length delimiting and bincode
#[pin_project]
pub struct RecvStream<In>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedRead<::quinn::RecvStream, LengthDelimitedCodec>,
        In,
        SymmetricalBincode<In>,
    >,
);

impl<In: DeserializeOwned> Stream for RecvStream<In> {
    type Item = result::Result<In, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

/// Error for open_bi. Currently just a quinn::ConnectionError
pub type OpenBiError = quinn::ConnectionError;

/// Error for accept_bi. Currently just a quinn::ConnectionError
pub type AcceptBiError = quinn::ConnectionError;

/// Types for quinn channels.
///
/// This exposes the types from quinn directly without attempting to wrap them.
#[derive(Debug, Clone, Copy)]
pub struct QuinnChannelTypes;

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<'a, In, Out>(
    #[pin] BoxFuture<'a, Result<SocketInner, quinn::ConnectionError>>,
    PhantomData<&'a (In, Out)>,
);

impl<'a, In, Out> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Self::Output> {
        match ready!(self.project().0.poll_unpin(cx)) {
            Ok((send, recv)) => {
                // turn chunks of bytes into a stream of messages using length delimited codec
                let send = FramedWrite::new(send, LengthDelimitedCodec::new());
                let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
                // turn stream of messages into a stream of bincode encoded messages
                let send = SymmetricallyFramed::new(send, SymmetricalBincode::default());
                let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::default());
                Poll::Ready(Ok((SendSink(send), RecvStream(recv))))
            }
            Err(_) => Poll::Ready(Err(quinn::ConnectionError::LocallyClosed)),
        }
    }
}

/// Future returned by accept_bi
#[pin_project]
pub struct AcceptBiFuture<'a, In, Out>(
    #[pin] flume::r#async::RecvFut<'a, SocketInner>,
    PhantomData<(In, Out)>,
);

impl<'a, In, Out> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.project().0.poll_unpin(cx).map(|conn| {
            let (send, recv) = conn.map_err(|e| {
                tracing::warn!("accept_bi: error receiving connection: {}", e);
                quinn::ConnectionError::LocallyClosed
            })?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            // box so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        })
    }
}

// pub type AcceptBiFuture<'a, In, Out> =
//     BoxFuture<'a, result::Result<self::Socket<In, Out>, self::AcceptBiError>>;

impl crate::ChannelTypes for QuinnChannelTypes {

    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type OpenBiError = self::OpenBiError;

    type AcceptBiError = self::OpenBiError;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::QuinnClientChannel<In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::QuinnServerChannel<In, Out>;
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ClientChannel<In, Out, QuinnChannelTypes>
    for self::QuinnClientChannel<In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        OpenBiFuture(
            async move {
                let (sender, receiver) = oneshot::channel();
                self.inner
                    .sender
                    .send_async(sender)
                    .await
                    .map_err(|_| quinn::ConnectionError::LocallyClosed)?;
                receiver
                    .await
                    .map_err(|_| quinn::ConnectionError::LocallyClosed)
            }
            .boxed(),
            PhantomData,
        )
    }
}

impl<In: RpcMessage + Sync, Out: RpcMessage + Sync> crate::ServerChannel<In, Out, QuinnChannelTypes>
    for self::QuinnServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(self.inner.receiver.recv_async(), PhantomData)
    }

    fn local_addr(&self) -> &[crate::LocalAddr] {
        &self.inner.local_addr
    }
}

#[derive(Debug)]
pub struct ServerSource<In: RpcMessage, Out: RpcMessage> {
    conn: quinn::Connection,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for ServerSource<In, Out> {
    type SendError = io::Error;
    type RecvError = io::Error;
    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> TypedConnection<In, Out> for ServerSource<In, Out> {
    type Channel = FramedBincode<QuinnSocket, In, Out>;

    type NextFut<'a> = BoxFuture<'a, result::Result<FramedBincode<QuinnSocket, In, Out>, quinn::ConnectionError>>;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let (send, recv) = self.conn.accept_bi().await?;
            let chan = QuinnSocket { send, recv };
            let wrapped = FramedBincode::new(chan, 1024 * 1024);
            Ok(wrapped)
        }.boxed()
    }
}

/// CreateChannelError for quinn channels.
#[derive(Debug, Clone)]
pub enum CreateChannelError {
    /// Something went wrong immediately when creating the quinn endpoint
    Io(io::ErrorKind, String),
    /// Error directly when calling connect on the quinn endpoint
    Connect(quinn::ConnectError),
    /// Error produced by the future returned by connect
    Connection(quinn::ConnectionError),
}

impl From<io::Error> for CreateChannelError {
    fn from(e: io::Error) -> Self {
        CreateChannelError::Io(e.kind(), e.to_string())
    }
}

impl From<quinn::ConnectionError> for CreateChannelError {
    fn from(e: quinn::ConnectionError) -> Self {
        CreateChannelError::Connection(e)
    }
}

impl From<quinn::ConnectError> for CreateChannelError {
    fn from(e: quinn::ConnectError) -> Self {
        CreateChannelError::Connect(e)
    }
}

impl fmt::Display for CreateChannelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for CreateChannelError {}

/// Combines both ends of a quinn strea, into a single socket, so we can transform it as one.
#[pin_project]
pub struct QuinnSocket {
    /// The send part of the quinn stream.
    #[pin]
    pub send: quinn::SendStream,
    /// The receive part of the quinn stream.
    #[pin]
    pub recv: quinn::RecvStream,
}

impl tokio::io::AsyncRead for QuinnSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().recv.poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for QuinnSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        self.project().send.poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<std::io::Result<()>> {
        self.project().send.poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        self.project().send.poll_shutdown(cx)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        self.project().send.poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.send.is_write_vectored()
    }
}
