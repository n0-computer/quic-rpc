//! QUIC transport implementation based on [quinn](https://crates.io/crates/quinn)
use std::{
    fmt, io,
    marker::PhantomData,
    net::SocketAddr,
    pin::Pin,
    result,
    sync::Arc,
    task::{Context, Poll},
};

use futures_lite::{Future, Stream, StreamExt};
use futures_sink::Sink;
use futures_util::FutureExt;
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::oneshot;
use tracing::{debug_span, Instrument};

use super::{
    util::{FramedPostcardRead, FramedPostcardWrite},
    StreamTypes,
};
use crate::{
    transport::{ConnectionErrors, Connector, Listener, LocalAddr},
    RpcMessage,
};

const MAX_FRAME_LENGTH: usize = 1024 * 1024 * 16;

#[derive(Debug)]
struct ListenerInner {
    endpoint: Option<quinn::Endpoint>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_addr: [LocalAddr; 1],
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ListenerInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping listener");
        if let Some(endpoint) = self.endpoint.take() {
            endpoint.close(0u32.into(), b"Listener dropped");

            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // spawn a task to wait for the endpoint to notify peers that it is closing
                let span = debug_span!("closing listener");
                handle.spawn(
                    async move {
                        endpoint.wait_idle().await;
                    }
                    .instrument(span),
                );
            }
        }
        if let Some(task) = self.task.take() {
            task.abort()
        }
    }
}

/// A listener using a quinn connection
#[derive(Debug)]
pub struct QuinnListener<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ListenerInner>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnListener<In, Out> {
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
            inner: Arc::new(ListenerInner {
                endpoint: Some(endpoint),
                task: Some(task),
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _p: PhantomData,
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
            inner: Arc::new(ListenerInner {
                endpoint: None,
                task: Some(task),
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _p: PhantomData,
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
            inner: Arc::new(ListenerInner {
                endpoint: None,
                task: None,
                local_addr: [LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnListener<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnListener<In, Out> {
    type SendError = io::Error;
    type RecvError = io::Error;
    type OpenError = quinn::ConnectionError;
    type AcceptError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for QuinnListener<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = self::SendSink<Out>;
    type RecvStream = self::RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Listener for QuinnListener<In, Out> {
    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), AcceptError> {
        let (send, recv) = self
            .inner
            .receiver
            .recv_async()
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)?;
        Ok((SendSink::new(send), RecvStream::new(recv)))
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
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // spawn a task to wait for the endpoint to notify peers that it is closing
                let span = debug_span!("closing client endpoint");
                handle.spawn(
                    async move {
                        endpoint.wait_idle().await;
                    }
                    .instrument(span),
                );
            }
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
pub struct QuinnConnector<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ClientConnectionInner>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> QuinnConnector<In, Out> {
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
        let reconnect = ReconnectHandler {
            endpoint,
            state: ConnectionState::NotConnected,
            addr,
            name,
        };
        tokio::pin!(reconnect);

        let mut receiver = Receiver::new(&requests);

        let mut pending_request: Option<
            oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>,
        > = None;
        let mut connection = None;

        enum Racer {
            Reconnect(Result<quinn::Connection, ReconnectErr>),
            Channel(Option<oneshot::Sender<Result<SocketInner, quinn::ConnectionError>>>),
        }

        loop {
            let mut conn_result = None;
            let mut chann_result = None;
            if !reconnect.connected() && pending_request.is_none() {
                match futures_lite::future::race(
                    reconnect.as_mut().map(Racer::Reconnect),
                    receiver.next().map(Racer::Channel),
                )
                .await
                {
                    Racer::Reconnect(connection_result) => conn_result = Some(connection_result),
                    Racer::Channel(channel_result) => {
                        chann_result = Some(channel_result);
                    }
                }
            } else if !reconnect.connected() {
                // only need a new connection
                conn_result = Some(reconnect.as_mut().await);
            } else if pending_request.is_none() {
                // there is a connection, just need a request
                chann_result = Some(receiver.next().await);
            }

            if let Some(conn_result) = conn_result {
                tracing::trace!("tick: connection result");
                match conn_result {
                    Ok(new_connection) => {
                        connection = Some(new_connection);
                    }
                    Err(e) => {
                        let connection_err = match e {
                            ReconnectErr::Connect(e) => {
                                // TODO(@divma): the type for now accepts only a
                                // ConnectionError, not a ConnectError. I'm mapping this now to
                                // some ConnectionError since before it was not even reported.
                                // Maybe adjust the type?
                                tracing::warn!(%e, "error calling connect");
                                quinn::ConnectionError::Reset
                            }
                            ReconnectErr::Connection(e) => {
                                tracing::warn!(%e, "failed to connect");
                                e
                            }
                        };
                        if let Some(request) = pending_request.take() {
                            if request.send(Err(connection_err)).is_err() {
                                tracing::debug!("requester dropped");
                            }
                        }
                    }
                }
            }

            if let Some(req) = chann_result {
                tracing::trace!("tick: bidi request");
                match req {
                    Some(request) => pending_request = Some(request),
                    None => {
                        tracing::debug!("client dropped");
                        if let Some(connection) = connection {
                            connection.close(0u32.into(), b"requester dropped");
                        }
                        break;
                    }
                }
            }

            if let Some(connection) = connection.as_mut() {
                if let Some(request) = pending_request.take() {
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
                            // NOTE: the connection might be stale, so we recreate the
                            // connection and set the request as pending instead of
                            // sending the error as a response
                            reconnect.set_not_connected();
                            pending_request = Some(request);
                        }
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
            _p: PhantomData,
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
            _p: PhantomData,
        }
    }
}

struct ReconnectHandler {
    endpoint: quinn::Endpoint,
    state: ConnectionState,
    addr: SocketAddr,
    name: String,
}

impl ReconnectHandler {
    pub fn set_not_connected(&mut self) {
        self.state.set_not_connected()
    }

    pub fn connected(&self) -> bool {
        matches!(self.state, ConnectionState::Connected(_))
    }
}

enum ConnectionState {
    /// There is no active connection. An attempt to connect will be made.
    NotConnected,
    /// Connecting to the remote.
    Connecting(quinn::Connecting),
    /// A connection is already established. In this state, no more connection attempts are made.
    Connected(quinn::Connection),
    /// Intermediate state while processing.
    Poisoned,
}

impl ConnectionState {
    pub fn poison(&mut self) -> ConnectionState {
        std::mem::replace(self, ConnectionState::Poisoned)
    }

    pub fn set_not_connected(&mut self) {
        *self = ConnectionState::NotConnected
    }
}

enum ReconnectErr {
    Connect(quinn::ConnectError),
    Connection(quinn::ConnectionError),
}

impl Future for ReconnectHandler {
    type Output = Result<quinn::Connection, ReconnectErr>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.state.poison() {
            ConnectionState::NotConnected => match self.endpoint.connect(self.addr, &self.name) {
                Ok(connecting) => {
                    self.state = ConnectionState::Connecting(connecting);
                    self.poll(cx)
                }
                Err(e) => {
                    self.state = ConnectionState::NotConnected;
                    Poll::Ready(Err(ReconnectErr::Connect(e)))
                }
            },
            ConnectionState::Connecting(mut connecting) => match Pin::new(&mut connecting).poll(cx)
            {
                Poll::Ready(res) => match res {
                    Ok(connection) => {
                        self.state = ConnectionState::Connected(connection.clone());
                        Poll::Ready(Ok(connection))
                    }
                    Err(e) => {
                        self.state = ConnectionState::NotConnected;
                        Poll::Ready(Err(ReconnectErr::Connection(e)))
                    }
                },
                Poll::Pending => {
                    self.state = ConnectionState::Connecting(connecting);
                    Poll::Pending
                }
            },
            ConnectionState::Connected(connection) => {
                self.state = ConnectionState::Connected(connection.clone());
                Poll::Ready(Ok(connection))
            }
            ConnectionState::Poisoned => unreachable!("poisoned connection state"),
        }
    }
}

/// Wrapper over [`flume::Receiver`] that can be used with [`tokio::select`].
///
/// NOTE: from https://github.com/zesterer/flume/issues/104:
/// > If RecvFut is dropped without being polled, the item is never received.
enum Receiver<'a, T>
where
    Self: 'a,
{
    PreReceive(&'a flume::Receiver<T>),
    Receiving(&'a flume::Receiver<T>, flume::r#async::RecvFut<'a, T>),
    Poisoned,
}

impl<'a, T> Receiver<'a, T> {
    fn new(recv: &'a flume::Receiver<T>) -> Self {
        Receiver::PreReceive(recv)
    }

    fn poison(&mut self) -> Self {
        std::mem::replace(self, Self::Poisoned)
    }
}

impl<T> Stream for Receiver<'_, T> {
    type Item = T;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.poison() {
            Receiver::PreReceive(recv) => {
                let fut = recv.recv_async();
                *self = Receiver::Receiving(recv, fut);
                self.poll_next(cx)
            }
            Receiver::Receiving(recv, mut fut) => match Pin::new(&mut fut).poll(cx) {
                Poll::Ready(Ok(t)) => {
                    *self = Receiver::PreReceive(recv);
                    Poll::Ready(Some(t))
                }
                Poll::Ready(Err(flume::RecvError::Disconnected)) => {
                    *self = Receiver::PreReceive(recv);
                    Poll::Ready(None)
                }
                Poll::Pending => {
                    *self = Receiver::Receiving(recv, fut);
                    Poll::Pending
                }
            },
            Receiver::Poisoned => unreachable!("poisoned receiver state"),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for QuinnConnector<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for QuinnConnector<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for QuinnConnector<In, Out> {
    type SendError = io::Error;
    type RecvError = io::Error;
    type OpenError = quinn::ConnectionError;
    type AcceptError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for QuinnConnector<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = self::SendSink<Out>;
    type RecvStream = self::RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Connector for QuinnConnector<In, Out> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .sender
            .send_async(sender)
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)?;
        let (send, recv) = receiver
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)??;
        Ok((SendSink::new(send), RecvStream::new(recv)))
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and postcard
///
/// If you want to send bytes directly, use [SendSink::into_inner] to get the
/// underlying [quinn::SendStream].
#[pin_project]
pub struct SendSink<Out>(#[pin] FramedPostcardWrite<quinn::SendStream, Out>);

impl<Out> fmt::Debug for SendSink<Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendSink").finish()
    }
}

impl<Out: Serialize> SendSink<Out> {
    fn new(inner: quinn::SendStream) -> Self {
        let inner = FramedPostcardWrite::new(inner, MAX_FRAME_LENGTH);
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
        Pin::new(&mut self.project().0).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        Pin::new(&mut self.project().0).start_send(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_flush(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_close(cx)
    }
}

/// A stream that wraps a quinn RecvStream with length delimiting and postcard
///
/// If you want to receive bytes directly, use [RecvStream::into_inner] to get
/// the underlying [quinn::RecvStream].
#[pin_project]
pub struct RecvStream<In>(#[pin] FramedPostcardRead<quinn::RecvStream, In>);

impl<In> fmt::Debug for RecvStream<In> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecvStream").finish()
    }
}

impl<In: DeserializeOwned> RecvStream<In> {
    fn new(inner: quinn::RecvStream) -> Self {
        let inner = FramedPostcardRead::new(inner, MAX_FRAME_LENGTH);
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
        Pin::new(&mut self.project().0).poll_next(cx)
    }
}

/// Error for open. Currently just a quinn::ConnectionError
pub type OpenError = quinn::ConnectionError;

/// Error for accept. Currently just a quinn::ConnectionError
pub type AcceptError = quinn::ConnectionError;

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
