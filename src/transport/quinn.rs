//! QUIC channel implementation based on quinn
use crate::{LocalAddr, RpcMessage};
use futures::channel::oneshot;
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{self, ready, Poll};
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

#[derive(Debug)]
struct ServerChannelInner {
    endpoint: quinn::Endpoint,
    task: Option<tokio::task::JoinHandle<result::Result<(), flume::SendError<SocketInner>>>>,
    local_addr: [LocalAddr; 1],
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ServerChannelInner {
    fn drop(&mut self) {
        self.endpoint.close(0u32.into(), b"server channel dropped");
        // this should not be necessary, since the task would terminate when the receiver is dropped.
        // but just to be on the safe side.
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
    async fn connection_handler(
        endpoint: quinn::Endpoint,
        sender: flume::Sender<SocketInner>,
    ) -> result::Result<(), flume::SendError<SocketInner>> {
        'outer: loop {
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
            loop {
                tracing::debug!("Awaiting incoming bidi substream on existing connection...");
                let bidi_stream = match conection.accept_bi().await {
                    Ok(bidi_stream) => bidi_stream,
                    Err(quinn::ConnectionError::ApplicationClosed(e)) => {
                        tracing::info!("Peer closed the connection {:?}", e);
                        continue 'outer;
                    }
                    Err(e) => {
                        tracing::warn!("Error opening stream: {}", e);
                        continue 'outer;
                    }
                };
                tracing::debug!("Sending substream to be handled... {}", bidi_stream.0.id());
                sender.send_async(bidi_stream).await?;
            }
        }
        Ok(())
    }

    /// Create a new server channel, given a quinn endpoint.
    pub fn new(endpoint: quinn::Endpoint) -> io::Result<Self> {
        let local_addr = endpoint.local_addr()?;
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::connection_handler(endpoint.clone(), sender));
        Ok(Self {
            inner: Arc::new(ServerChannelInner {
                endpoint,
                task: Some(task),
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        })
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

type SocketInner = (quinn::SendStream, quinn::RecvStream);

#[derive(Debug)]
struct ClientChannelInner {
    /// The quinn endpoint, we just keep a clone of this for information
    endpoint: quinn::Endpoint,
    /// The task that handles creating new connections
    task: Option<tokio::task::JoinHandle<()>>,
    /// The channel to receive new connections
    sender: flume::Sender<oneshot::Sender<SocketInner>>,
}

impl Drop for ClientChannelInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping client channel");
        self.endpoint.close(0u32.into(), b"client channel dropped");
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
        requests: flume::Receiver<oneshot::Sender<SocketInner>>
    ) {
        if let Err(res) = Self::connection_handler_inner(endpoint, addr, name, requests).await {
            tracing::info!("Connection handler finished: {:?}", res);
        } else {
            unreachable!()
        }
    }

    /// Create a new channel
    pub fn new(endpoint: quinn::Endpoint, addr: SocketAddr, name: String) -> Self {
        let (sender, receiver) = flume::bounded(4);
        let task = tokio::spawn(Self::connection_handler(
            endpoint.clone(),
            addr,
            name,
            receiver,
        ));
        Self {
            inner: Arc::new(ClientChannelInner {
                endpoint,
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
    #[pin] oneshot::Receiver<SocketInner>,
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
        let (sender, receiver) = oneshot::channel();
        // todo: make this async
        self.inner.sender.send(sender).unwrap();
        OpenBiFuture(receiver, PhantomData)
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
