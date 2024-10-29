//! iroh-net transport implementation based on [iroh-net](https://crates.io/crates/iroh-net)

use crate::{
    transport::{Connection, ConnectionErrors, LocalAddr, ServerEndpoint},
    RpcMessage,
};

use std::{
    fmt,
    future::Future,
    io,
    iter::once,
    marker::PhantomData,
    net::SocketAddr,
    pin::pin,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

use futures_lite::{Stream, StreamExt};
use futures_sink::Sink;
use futures_util::FutureExt;
use iroh_net::NodeAddr;
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use tokio::sync::oneshot;
use tracing::{debug_span, Instrument};

use super::{
    util::{FramedBincodeRead, FramedBincodeWrite},
    ConnectionCommon,
};

const MAX_FRAME_LENGTH: usize = 1024 * 1024 * 16;

#[derive(Debug)]
struct ServerEndpointInner {
    endpoint: Option<iroh_net::Endpoint>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_addr: Vec<LocalAddr>,
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ServerEndpointInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping server endpoint");
        if let Some(endpoint) = self.endpoint.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // spawn a task to wait for the endpoint to notify peers that it is closing
                let span = debug_span!("closing server endpoint");
                handle.spawn(
                    async move {
                        // iroh-net endpoint's close is async, and internally it waits the
                        // underlying quinn endpoint to be idle.
                        if let Err(e) = endpoint
                            .close(0u32.into(), b"server endpoint dropped")
                            .await
                        {
                            tracing::warn!(?e, "error closing server endpoint");
                        }
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

/// A server endpoint using a quinn connection
#[derive(Debug)]
pub struct IrohNetServerEndpoint<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ServerEndpointInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> IrohNetServerEndpoint<In, Out> {
    /// handles RPC requests from a connection
    ///
    /// to cleanly shut down the handler, drop the receiver side of the sender.
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

    async fn endpoint_handler(endpoint: iroh_net::Endpoint, sender: flume::Sender<SocketInner>) {
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
    pub fn new(endpoint: iroh_net::Endpoint) -> io::Result<Self> {
        let (ipv4_socket_addr, maybe_ipv6_socket_addr) = endpoint.bound_sockets();
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::endpoint_handler(endpoint.clone(), sender));
        Ok(Self {
            inner: Arc::new(ServerEndpointInner {
                endpoint: Some(endpoint),
                task: Some(task),
                local_addr: once(LocalAddr::Socket(ipv4_socket_addr))
                    .chain(maybe_ipv6_socket_addr.map(LocalAddr::Socket))
                    .collect(),
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
                local_addr: vec![LocalAddr::Socket(local_addr)],
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
                local_addr: vec![LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for IrohNetServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for IrohNetServerEndpoint<In, Out> {
    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out> for IrohNetServerEndpoint<In, Out> {
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint<In, Out> for IrohNetServerEndpoint<In, Out> {
    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), AcceptBiError> {
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
    endpoint: Option<iroh_net::Endpoint>,
    /// The task that handles creating new connections
    task: Option<tokio::task::JoinHandle<()>>,
    /// The channel to send new received connections
    connections_tx: flume::Sender<oneshot::Sender<anyhow::Result<SocketInner>>>,
}

impl Drop for ClientConnectionInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping client connection");
        if let Some(endpoint) = self.endpoint.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // spawn a task to wait for the endpoint to notify peers that it is closing
                let span = debug_span!("closing client endpoint");
                handle.spawn(
                    async move {
                        // iroh-net endpoint's close is async, and internally it waits the
                        // underlying quinn endpoint to be idle.
                        if let Err(e) = endpoint
                            .close(0u32.into(), b"client connection dropped")
                            .await
                        {
                            tracing::warn!(?e, "error closing client endpoint");
                        }
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

/// A connection using an iroh-net connection
pub struct IrohNetConnection<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ClientConnectionInner>,
    _phantom: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> IrohNetConnection<In, Out> {
    async fn single_connection_handler(
        connection: quinn::Connection,
        requests_rx: flume::Receiver<oneshot::Sender<anyhow::Result<SocketInner>>>,
    ) {
        loop {
            tracing::debug!("Awaiting request for new bidi substream...");
            let Ok(request_tx) = requests_rx.recv_async().await else {
                tracing::info!("Single connection handler finished");
                return;
            };

            tracing::debug!("Got request for new bidi substream");
            match connection.open_bi().await {
                Ok(pair) => {
                    tracing::debug!("Bidi substream opened");
                    if request_tx.send(Ok(pair)).is_err() {
                        tracing::debug!("requester dropped");
                    }
                }
                Err(e) => {
                    tracing::warn!(?e, "error opening bidi substream");
                    if request_tx
                        .send(anyhow::Context::context(
                            Err(e),
                            "error opening bidi substream",
                        ))
                        .is_err()
                    {
                        tracing::debug!("requester dropped");
                    }
                }
            }
        }
    }

    /// Client connection handler.
    ///
    /// It will run until the send side of the channel is dropped.
    /// All other errors are logged and handled internally.
    /// It will try to keep a connection open at all times.
    async fn reconnect_handler_inner(
        endpoint: iroh_net::Endpoint,
        node_addr: NodeAddr,
        alpn: Vec<u8>,
        requests: flume::Receiver<oneshot::Sender<anyhow::Result<SocketInner>>>,
    ) {
        let mut reconnect = pin!(ReconnectHandler {
            endpoint,
            state: ConnectionState::NotConnected,
            node_addr,
            alpn,
        });

        let mut receiver = Receiver::new(&requests);

        let mut pending_request: Option<oneshot::Sender<anyhow::Result<SocketInner>>> = None;
        let mut connection = None;

        enum Racer {
            Reconnect(anyhow::Result<quinn::Connection>),
            Channel(Option<oneshot::Sender<anyhow::Result<SocketInner>>>),
        }

        loop {
            let mut conn_result = None;
            let mut chan_result = None;
            if !reconnect.connected() && pending_request.is_none() {
                match futures_lite::future::race(
                    reconnect.as_mut().map(Racer::Reconnect),
                    receiver.next().map(Racer::Channel),
                )
                .await
                {
                    Racer::Reconnect(connection_result) => conn_result = Some(connection_result),
                    Racer::Channel(channel_result) => {
                        chan_result = Some(channel_result);
                    }
                }
            } else if !reconnect.connected() {
                // only need a new connection
                conn_result = Some(reconnect.as_mut().await);
            } else if pending_request.is_none() {
                // there is a connection, just need a request
                chan_result = Some(receiver.next().await);
            }

            if let Some(conn_result) = conn_result {
                tracing::trace!("tick: connection result");
                match conn_result {
                    Ok(new_connection) => {
                        connection = Some(new_connection);
                    }
                    Err(e) => {
                        if let Some(request) = pending_request.take() {
                            if request.send(Err(e)).is_err() {
                                tracing::debug!("requester dropped");
                            }
                        }
                    }
                }
            }

            if let Some(req) = chan_result {
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
        endpoint: iroh_net::Endpoint,
        addr: NodeAddr,
        alpn: Vec<u8>,
        requests: flume::Receiver<oneshot::Sender<anyhow::Result<SocketInner>>>,
    ) {
        Self::reconnect_handler_inner(endpoint, addr, alpn, requests).await;
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
                connections_tx: sender,
            }),
            _phantom: PhantomData,
        }
    }

    /// Create a new channel
    pub fn new(
        endpoint: iroh_net::Endpoint,
        node_addr: impl Into<NodeAddr>,
        alpn: Vec<u8>,
    ) -> Self {
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::reconnect_handler(
            endpoint.clone(),
            node_addr.into(),
            alpn,
            receiver,
        ));
        Self {
            inner: Arc::new(ClientConnectionInner {
                endpoint: Some(endpoint),
                task: Some(task),
                connections_tx: sender,
            }),
            _phantom: PhantomData,
        }
    }
}

#[pin_project]
struct ReconnectHandler {
    endpoint: iroh_net::Endpoint,
    #[pin]
    state: ConnectionState,
    node_addr: NodeAddr,
    alpn: Vec<u8>,
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
    Connecting(Pin<Box<dyn Future<Output = anyhow::Result<quinn::Connection>> + Send>>),
    /// A connection is already established. In this state, no more connection attempts are made.
    Connected(quinn::Connection),
    /// Intermediate state while processing.
    Poisoned,
}

impl ConnectionState {
    pub fn poison(&mut self) -> Self {
        std::mem::replace(self, Self::Poisoned)
    }

    pub fn set_not_connected(&mut self) {
        *self = Self::NotConnected
    }
}

impl Future for ReconnectHandler {
    type Output = anyhow::Result<quinn::Connection>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match self.state.poison() {
            ConnectionState::NotConnected => {
                self.state = ConnectionState::Connecting(Box::pin({
                    let endpoint = self.endpoint.clone();
                    let node_addr = self.node_addr.clone();
                    let alpn = self.alpn.clone();
                    async move { endpoint.connect(node_addr, &alpn).await }
                }));
                self.poll(cx)
            }

            ConnectionState::Connecting(mut connecting) => match connecting.as_mut().poll(cx) {
                Poll::Ready(res) => match res {
                    Ok(connection) => {
                        self.state = ConnectionState::Connected(connection.clone());
                        Poll::Ready(Ok(connection))
                    }
                    Err(e) => {
                        self.state = ConnectionState::NotConnected;
                        Poll::Ready(Err(e))
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

impl<'a, T> Stream for Receiver<'a, T> {
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

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for IrohNetConnection<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for IrohNetConnection<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _phantom: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for IrohNetConnection<In, Out> {
    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenError = anyhow::Error;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out> for IrohNetConnection<In, Out> {
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Connection<In, Out> for IrohNetConnection<In, Out> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        let (sender, receiver) = oneshot::channel();
        self.inner
            .connections_tx
            .send_async(sender)
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)?;
        let (send, recv) = receiver
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)??;
        Ok((SendSink::new(send), RecvStream::new(recv)))
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

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        Pin::new(&mut self.project().0).start_send(item)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_close(cx)
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
    type Item = Result<In, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.project().0).poll_next(cx)
    }
}

/// Error for open. Currently just an anyhow::Error
pub type OpenBiError = anyhow::Error;

/// Error for accept. Currently just a quinn::ConnectionError
pub type AcceptBiError = quinn::ConnectionError;
