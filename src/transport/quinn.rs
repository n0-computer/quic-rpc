//! QUIC transport implementation based on [quinn](https://crates.io/crates/quinn)
use crate::{
    transport::{Connection, ConnectionErrors, LocalAddr, ServerEndpoint},
    RpcMessage,
};
use futures::channel::oneshot;
use futures::{Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::{fmt, io, marker::PhantomData, pin::Pin, result};
use tracing::{debug_span, Instrument};

use super::{
    util::{FramedBincodeRead, FramedBincodeWrite},
    ConnectionCommon,
};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

const MAX_FRAME_LENGTH: usize = 1024 * 1024 * 16;

#[derive(Debug)]
struct ServerEndpointInner {
    endpoint: Option<quinn::Endpoint>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_addr: [LocalAddr; 1],
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ServerEndpointInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping server endpoint");
        if let Some(endpoint) = self.endpoint.take() {
            let span = debug_span!("closing server endpoint");
            endpoint.close(0u32.into(), b"server endpoint dropped");
            // spawn a task to wait for the endpoint to notify peers that it is closing
            tokio::spawn(
                async move {
                    endpoint.wait_idle().await;
                }
                .instrument(span),
            );
        }
        if let Some(task) = self.task.take() {
            task.abort()
        }
    }
}

/// A server endpoint using a quinn connection
#[derive(Debug)]
pub struct QuinnServerEndpoint<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ServerEndpointInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnServerEndpoint<In, Out> {
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
                    tracing::debug!("Error accepting stream: {}", e);
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
            tracing::debug!("Waiting for incoming connection...");
            let connecting = match endpoint.accept().await {
                Some(connecting) => connecting,
                None => break,
            };
            tracing::debug!("Awaiting connection from connect...");
            let conection = match connecting.await {
                Ok(conection) => conection,
                Err(e) => {
                    tracing::warn!("Error accepting connection: {}", e);
                    continue;
                }
            };
            tracing::debug!(
                "Connection established from {:?}",
                conection.remote_address()
            );
            tracing::debug!("Spawning connection handler...");
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
            inner: Arc::new(ServerEndpointInner {
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
            inner: Arc::new(ServerEndpointInner {
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
            inner: Arc::new(ServerEndpointInner {
                endpoint: None,
                task: None,
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnServerEndpoint<In, Out> {
    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out> for QuinnServerEndpoint<In, Out> {
    type RecvStream = self::RecvStream<In>;
    type SendSink = self::SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint<In, Out> for QuinnServerEndpoint<In, Out> {
    type AcceptBiFut = AcceptBiFuture<In, Out>;

    fn accept_bi(&self) -> Self::AcceptBiFut {
        AcceptBiFuture(self.inner.receiver.clone().into_recv_async(), PhantomData)
    }

    fn local_addr(&self) -> &[LocalAddr] {
        &self.inner.local_addr
    }
}

type SocketInner = (quinn::SendStream, quinn::RecvStream);

#[derive(Debug)]
struct ClientConnectionInner {
    /// The quinn endpoint, we just keep a clone of this for information
    endpoint: Option<quinn::Endpoint>,
    /// The task that handles creating new connections
    task: Option<tokio::task::JoinHandle<()>>,
    /// The channel to receive new connections
    sender: flume::Sender<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>,
}

impl Drop for ClientConnectionInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping client connection");
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close(0u32.into(), b"client connection dropped");
            // spawn a task to wait for the endpoint to notify peers that it is closing
            let span = debug_span!("closing client endpoint");
            tokio::spawn(
                async move {
                    endpoint.wait_idle().await;
                }
                .instrument(span),
            );
        }
        // this should not be necessary, since the task would terminate when the receiver is dropped.
        // but just to be on the safe side.
        if let Some(task) = self.task.take() {
            tracing::debug!("Aborting task");
            task.abort();
        }
    }
}

/// A connection using a quinn connection
pub struct QuinnConnection<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ClientConnectionInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnConnection<In, Out> {
    async fn single_connection_handler_inner(
        connection: quinn::Connection,
        requests: flume::Receiver<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>,
    ) -> result::Result<(), flume::RecvError> {
        loop {
            tracing::debug!("Awaiting request for new bidi substream...");
            let request = requests.recv_async().await?;
            tracing::debug!("Got request for new bidi substream");
            match connection.open_bi().await {
                Ok(pair) => {
                    tracing::debug!("Bidi substream opened");
                    if request.send(Ok(pair)).is_err() {
                        tracing::debug!("requester dropped");
                    }
                }
                Err(e) => {
                    tracing::warn!("error opening bidi substream: {}", e);
                    if request.send(Err(e)).is_err() {
                        tracing::debug!("requester dropped");
                    }
                }
            }
        }
    }

    async fn single_connection_handler(
        connection: quinn::Connection,
        requests: flume::Receiver<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>,
    ) {
        if Self::single_connection_handler_inner(connection, requests)
            .await
            .is_err()
        {
            tracing::info!("Single connection handler finished");
        } else {
            unreachable!()
        }
    }

    /// Client connection handler.
    ///
    /// It will run until the send side of the channel is dropped.
    /// All other errors are logged and handled internally.
    /// It will try to keep a connection open at all times.
    async fn reconnect_handler_inner(
        endpoint: quinn::Endpoint,
        addr: SocketAddr,
        name: String,
        requests: flume::Receiver<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>,
    ) {
        // a pending request to open a bi-directional stream that was received with a lost
        // connection
        let mut pending_request = None;
        'outer: loop {
            tracing::debug!("Connecting to {} as {}", addr, name);
            let connecting = match endpoint.connect(addr, &name) {
                Ok(connecting) => connecting,
                Err(e) => {
                    tracing::warn!("error calling connect: {}", e);
                    // could not connect, if a request is pending, drop it.
                    pending_request = None;
                    // try again. Maybe delay?
                    continue;
                }
            };
            let connection = match connecting.await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::warn!("error awaiting connect: {}", e);
                    // could not connect, if a request is pending, drop it.
                    pending_request = None;
                    // try again. Maybe delay?
                    continue;
                }
            };
            loop {
                // first handle the pending request, then check for new requests
                let request = match pending_request.take() {
                    Some(request) => request,
                    None => {
                        tracing::debug!("Awaiting request for new bidi substream...");
                        match requests.recv_async().await {
                            Ok(request) => request,
                            Err(_) => {
                                tracing::debug!("client dropped");
                                connection.close(0u32.into(), b"requester dropped");
                                break;
                            }
                        }
                    }
                };
                tracing::debug!("Got request for new bidi substream");
                match connection.open_bi().await {
                    Ok(pair) => {
                        tracing::debug!("Bidi substream opened");
                        if request.send(Ok(pair)).is_err() {
                            tracing::debug!("requester dropped");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("error opening bidi substream: {}", e);
                        tracing::warn!("recreating connection");
                        pending_request = Some(request);
                        // try again. Maybe delay?
                        continue 'outer;
                    }
                }
            }
        }
    }

    async fn reconnect_handler(
        endpoint: quinn::Endpoint,
        addr: SocketAddr,
        name: String,
        requests: flume::Receiver<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>,
    ) {
        Self::reconnect_handler_inner(endpoint, addr, name, requests).await;
        tracing::info!("Reconnect handler finished");
    }

    /// Create a new channel
    pub fn from_connection(connection: quinn::Connection) -> Self {
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::single_connection_handler(connection, receiver));
        Self {
            inner: Arc::new(ClientConnectionInner {
                endpoint: None,
                task: Some(task),
                sender,
            }),
            _phantom: PhantomData,
        }
    }

    /// Create a new channel
    pub fn new(endpoint: quinn::Endpoint, addr: SocketAddr, name: String) -> Self {
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::reconnect_handler(
            endpoint.clone(),
            addr,
            name,
            receiver,
        ));
        Self {
            inner: Arc::new(ClientConnectionInner {
                endpoint: Some(endpoint),
                task: Some(task),
                sender,
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for QuinnConnection<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnConnection<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnConnection<In, Out> {
    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out> for QuinnConnection<In, Out> {
    type SendSink = self::SendSink<Out>;
    type RecvStream = self::RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Connection<In, Out> for QuinnConnection<In, Out> {
    type OpenBiFut = OpenBiFuture<In, Out>;

    fn open_bi(&self) -> Self::OpenBiFut {
        let (sender, receiver) = oneshot::channel();
        OpenBiFuture(
            OpenBiFutureState::Sending(self.inner.sender.clone().into_send_async(sender), receiver),
            PhantomData,
        )
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
///
/// If you want to send bytes directly, use [SendSink::into_inner] to get the
/// underlying [quinn::SendStream].
#[pin_project]
pub struct SendSink<Out>(#[pin] FramedBincodeWrite<quinn::SendStream, Out>);

impl<Out> fmt::Debug for SendSink<Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendSink").finish()
    }
}

impl<Out: Serialize> SendSink<Out> {
    fn new(inner: quinn::SendStream) -> Self {
        let inner = FramedBincodeWrite::new(inner, MAX_FRAME_LENGTH);
        Self(inner)
    }
}

impl<Out> SendSink<Out> {
    /// Get the underlying [quinn::SendStream], which implements
    /// [tokio::io::AsyncWrite] and can be used to send bytes directly.
    pub fn into_inner(self) -> quinn::SendStream {
        self.0.into_inner()
    }
}

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
///
/// If you want to receive bytes directly, use [RecvStream::into_inner] to get
/// the underlying [quinn::RecvStream].
#[pin_project]
pub struct RecvStream<In>(#[pin] FramedBincodeRead<quinn::RecvStream, In>);

impl<In> fmt::Debug for RecvStream<In> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream").finish()
    }
}

impl<In: DeserializeOwned> RecvStream<In> {
    fn new(inner: quinn::RecvStream) -> Self {
        let inner = FramedBincodeRead::new(inner, MAX_FRAME_LENGTH);
        Self(inner)
    }
}

impl<In> RecvStream<In> {
    /// Get the underlying [quinn::RecvStream], which implements
    /// [tokio::io::AsyncRead] and can be used to receive bytes directly.
    pub fn into_inner(self) -> quinn::RecvStream {
        self.0.into_inner()
    }
}

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

enum OpenBiFutureState {
    /// Sending the oneshot sender to the server
    Sending(
        flume::r#async::SendFut<
            'static,
            oneshot::Sender<Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>>,
        >,
        oneshot::Receiver<Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>>,
    ),
    /// Receiving the channel from the server
    Receiving(
        oneshot::Receiver<Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError>>,
    ),
    /// Taken or done
    Taken,
}

impl OpenBiFutureState {
    fn take(&mut self) -> Self {
        std::mem::replace(self, Self::Taken)
    }
}

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<In, Out>(OpenBiFutureState, PhantomData<(In, Out)>);

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for OpenBiFuture<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenBiFuture").finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<In, Out> {
    type Output = result::Result<self::Socket<In, Out>, self::OpenBiError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.0.take() {
            OpenBiFutureState::Sending(mut fut, recever) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok(_)) => {
                    self.0 = OpenBiFutureState::Receiving(recever);
                    self.poll(cx)
                }
                Poll::Pending => {
                    self.0 = OpenBiFutureState::Sending(fut, recever);
                    Poll::Pending
                }
                Poll::Ready(Err(_)) => Poll::Ready(Err(quinn::ConnectionError::LocallyClosed)),
            },
            OpenBiFutureState::Receiving(mut fut) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok(Ok((send, recv)))) => {
                    let send = SendSink::new(send);
                    let recv = RecvStream::new(recv);
                    Poll::Ready(Ok((send, recv)))
                }
                Poll::Ready(Ok(Err(cause))) => Poll::Ready(Err(cause)),
                Poll::Pending => {
                    self.0 = OpenBiFutureState::Receiving(fut);
                    Poll::Pending
                }
                Poll::Ready(Err(_)) => Poll::Ready(Err(quinn::ConnectionError::LocallyClosed)),
            },
            OpenBiFutureState::Taken => unreachable!(),
        }
    }
}

/// Future returned by accept_bi
#[pin_project]
pub struct AcceptBiFuture<In, Out>(
    #[pin] flume::r#async::RecvFut<'static, SocketInner>,
    PhantomData<(In, Out)>,
);

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for AcceptBiFuture<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AcceptBiFuture").finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Future for AcceptBiFuture<In, Out> {
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
            let send = SendSink::new(send);
            let recv = RecvStream::new(recv);
            Ok((send, recv))
        })
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

/// Get the handshake data from a quinn connection that uses rustls.
pub fn get_handshake_data(
    connection: &quinn::Connection,
) -> Option<quinn::crypto::rustls::HandshakeData> {
    let handshake_data = connection.handshake_data()?;
    let tls_connection = handshake_data.downcast_ref::<quinn::crypto::rustls::HandshakeData>()?;
    Some(quinn::crypto::rustls::HandshakeData {
        protocol: tls_connection.protocol.clone(),
        server_name: tls_connection.server_name.clone(),
    })
}
