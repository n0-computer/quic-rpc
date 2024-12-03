//! iroh transport implementation based on [iroh](https://crates.io/crates/iroh)

use std::{
    collections::BTreeSet,
    fmt,
    future::Future,
    io,
    iter::once,
    marker::PhantomData,
    net::SocketAddr,
    pin::{pin, Pin},
    sync::Arc,
    task::{Context, Poll},
};

use flume::TryRecvError;
use futures_lite::Stream;
use futures_sink::Sink;
use iroh::{NodeAddr, NodeId};
use pin_project::pin_project;
use quinn::Connection;
use serde::{de::DeserializeOwned, Serialize};
use tokio::{sync::oneshot, task::yield_now};
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
    endpoint: Option<iroh::Endpoint>,
    task: Option<tokio::task::JoinHandle<()>>,
    local_addr: Vec<LocalAddr>,
    receiver: flume::Receiver<SocketInner>,
}

impl Drop for ListenerInner {
    fn drop(&mut self) {
        tracing::debug!("Dropping server endpoint");
        if let Some(endpoint) = self.endpoint.take() {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                // spawn a task to wait for the endpoint to notify peers that it is closing
                let span = debug_span!("closing listener");
                handle.spawn(
                    async move {
                        // iroh endpoint's close is async, and internally it waits the
                        // underlying quinn endpoint to be idle.
                        if let Err(e) = endpoint.close().await {
                            tracing::warn!(?e, "error closing listener");
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

/// Access control for the server, either unrestricted or limited to a list of nodes that can
/// connect to the server endpoint
#[derive(Debug, Clone)]
pub enum AccessControl {
    /// Unrestricted access, anyone can connect
    Unrestricted,
    /// Restricted access, only nodes in the list can connect, all other nodes will be rejected
    Allowed(Vec<NodeId>),
}

/// A server endpoint using a quinn connection
#[derive(Debug)]
pub struct IrohListener<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ListenerInner>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> IrohListener<In, Out> {
    /// handles RPC requests from a connection
    ///
    /// to cleanly shut down the handler, drop the receiver side of the sender.
    async fn connection_handler(connection: quinn::Connection, sender: flume::Sender<SocketInner>) {
        loop {
            tracing::debug!("Awaiting incoming bidi substream on existing connection...");
            let bidi_stream = match connection.accept_bi().await {
                Ok(bidi_stream) => bidi_stream,
                Err(quinn::ConnectionError::ApplicationClosed(e)) => {
                    tracing::debug!(?e, "Peer closed the connection");
                    break;
                }
                Err(e) => {
                    tracing::debug!(?e, "Error accepting stream");
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

    async fn endpoint_handler(
        endpoint: iroh::Endpoint,
        sender: flume::Sender<SocketInner>,
        allowed_node_ids: BTreeSet<NodeId>,
    ) {
        loop {
            tracing::debug!("Waiting for incoming connection...");
            let connecting = match endpoint.accept().await {
                Some(connecting) => connecting,
                None => break,
            };

            tracing::debug!("Awaiting connection from connect...");
            let connection = match connecting.await {
                Ok(connection) => connection,
                Err(e) => {
                    tracing::warn!(?e, "Error accepting connection");
                    continue;
                }
            };

            // When the `allowed_node_ids` is empty, it's empty forever, so the CPU's branch
            // prediction should always optimize this block away from this loop.
            // The same applies when it isn't empty, ignoring the check for emptiness and always
            // extracting the node id and checking if it's in the set.
            if !allowed_node_ids.is_empty() {
                let Ok(client_node_id) =
                    iroh::endpoint::get_remote_node_id(&connection).map_err(|e| {
                        tracing::error!(
                            ?e,
                            "Failed to extract iroh node id from incoming connection from {:?}",
                            connection.remote_address()
                        )
                    })
                else {
                    connection.close(0u32.into(), b"failed to extract iroh node id");
                    continue;
                };

                if !allowed_node_ids.contains(&client_node_id) {
                    connection.close(0u32.into(), b"forbidden node id");
                    continue;
                }
            }

            tracing::debug!(
                "Connection established from {:?}",
                connection.remote_address()
            );

            tracing::debug!("Spawning connection handler...");
            tokio::spawn(Self::connection_handler(connection, sender.clone()));
        }
    }

    /// Create a new server channel, given a quinn endpoint, with unrestricted access by node id
    ///
    /// The server channel will take care of listening on the endpoint and spawning
    /// handlers for new connections.
    pub fn new(endpoint: iroh::Endpoint) -> io::Result<Self> {
        Self::new_with_access_control(endpoint, AccessControl::Unrestricted)
    }

    /// Create a new server endpoint, with specified access control
    ///
    /// The server channel will take care of listening on the endpoint and spawning
    /// handlers for new connections.
    pub fn new_with_access_control(
        endpoint: iroh::Endpoint,
        access_control: AccessControl,
    ) -> io::Result<Self> {
        let allowed_node_ids = match access_control {
            AccessControl::Unrestricted => BTreeSet::new(),
            AccessControl::Allowed(list) if list.is_empty() => {
                return Err(io::Error::other(
                    "Empty list of allowed nodes, \
                    endpoint would reject all connections",
                ));
            }
            AccessControl::Allowed(list) => BTreeSet::from_iter(list),
        };

        let (ipv4_socket_addr, maybe_ipv6_socket_addr) = endpoint.bound_sockets();
        let (sender, receiver) = flume::bounded(16);
        let task = tokio::spawn(Self::endpoint_handler(
            endpoint.clone(),
            sender,
            allowed_node_ids,
        ));

        Ok(Self {
            inner: Arc::new(ListenerInner {
                endpoint: Some(endpoint),
                task: Some(task),
                local_addr: once(LocalAddr::Socket(ipv4_socket_addr))
                    .chain(maybe_ipv6_socket_addr.map(LocalAddr::Socket))
                    .collect(),
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
                local_addr: vec![LocalAddr::Socket(local_addr)],
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
                local_addr: vec![LocalAddr::Socket(local_addr)],
                receiver,
            }),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for IrohListener<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for IrohListener<In, Out> {
    type SendError = io::Error;
    type RecvError = io::Error;
    type OpenError = quinn::ConnectionError;
    type AcceptError = quinn::ConnectionError;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for IrohListener<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Listener for IrohListener<In, Out> {
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
    endpoint: Option<iroh::Endpoint>,
    /// The task that handles creating new connections
    task: Option<tokio::task::JoinHandle<()>>,
    /// The channel to send new received connections
    requests_tx: flume::Sender<oneshot::Sender<anyhow::Result<SocketInner>>>,
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
                        // iroh endpoint's close is async, and internally it waits the
                        // underlying quinn endpoint to be idle.
                        if let Err(e) = endpoint.close().await {
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

/// A connection using an iroh connection
pub struct IrohConnector<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<ClientConnectionInner>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> IrohConnector<In, Out> {
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
        endpoint: iroh::Endpoint,
        node_addr: NodeAddr,
        alpn: Vec<u8>,
        requests_rx: flume::Receiver<oneshot::Sender<anyhow::Result<SocketInner>>>,
    ) {
        let mut reconnect = pin!(ReconnectHandler {
            endpoint,
            state: ConnectionState::NotConnected,
            node_addr,
            alpn,
        });

        let mut pending_request: Option<oneshot::Sender<anyhow::Result<SocketInner>>> = None;
        let mut connection: Option<Connection> = None;

        loop {
            // First we check if there is already a request ready in the channel
            if pending_request.is_none() {
                pending_request = match requests_rx.try_recv() {
                    Ok(req) => Some(req),
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        tracing::debug!("client dropped");
                        if let Some(connection) = connection {
                            connection.close(0u32.into(), b"requester dropped");
                        }
                        break;
                    }
                };
            }

            // If not connected, we attempt to establish a connection
            if !reconnect.connected() {
                tracing::trace!("tick: connection result");
                match reconnect.as_mut().await {
                    Ok(new_connection) => {
                        connection = Some(new_connection);
                    }
                    Err(e) => {
                        // If there was a pending request, we error it out as we're not connected
                        if let Some(request_ack_tx) = pending_request.take() {
                            if request_ack_tx.send(Err(e)).is_err() {
                                tracing::debug!("requester dropped");
                            }
                        }

                        // Yielding back to the runtime, otherwise this can run on a busy loop
                        // due to the always ready nature of things, messing up with single thread
                        // runtime flavor of tokio
                        yield_now().await;
                    }
                }
                // If we didn't have a ready request in the channel, we wait for one
            } else if pending_request.is_none() {
                let Ok(req) = requests_rx.recv_async().await else {
                    tracing::debug!("client dropped");
                    if let Some(connection) = connection {
                        connection.close(0u32.into(), b"requester dropped");
                    }
                    break;
                };

                tracing::trace!("tick: bidi request");
                pending_request = Some(req);
            }

            // If we have a connection and a pending request, we good, just process it
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
                            tracing::warn!(?e, "error opening bidi substream");
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
        endpoint: iroh::Endpoint,
        addr: NodeAddr,
        alpn: Vec<u8>,
        requests_rx: flume::Receiver<oneshot::Sender<anyhow::Result<SocketInner>>>,
    ) {
        Self::reconnect_handler_inner(endpoint, addr, alpn, requests_rx).await;
        tracing::info!("Reconnect handler finished");
    }

    /// Create a new channel
    pub fn from_connection(connection: quinn::Connection) -> Self {
        let (requests_tx, requests_rx) = flume::bounded(16);
        let task = tokio::spawn(Self::single_connection_handler(connection, requests_rx));
        Self {
            inner: Arc::new(ClientConnectionInner {
                endpoint: None,
                task: Some(task),
                requests_tx,
            }),
            _p: PhantomData,
        }
    }

    /// Create a new channel
    pub fn new(endpoint: iroh::Endpoint, node_addr: impl Into<NodeAddr>, alpn: Vec<u8>) -> Self {
        let (requests_tx, requests_rx) = flume::bounded(16);
        let task = tokio::spawn(Self::reconnect_handler(
            endpoint.clone(),
            node_addr.into(),
            alpn,
            requests_rx,
        ));
        Self {
            inner: Arc::new(ClientConnectionInner {
                endpoint: Some(endpoint),
                task: Some(task),
                requests_tx,
            }),
            _p: PhantomData,
        }
    }
}

struct ReconnectHandler {
    endpoint: iroh::Endpoint,
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

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for IrohConnector<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("inner", &self.inner)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for IrohConnector<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for IrohConnector<In, Out> {
    type SendError = io::Error;
    type RecvError = io::Error;
    type OpenError = anyhow::Error;
    type AcceptError = anyhow::Error;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for IrohConnector<In, Out> {
    type In = In;
    type Out = Out;
    type SendSink = SendSink<Out>;
    type RecvStream = RecvStream<In>;
}

impl<In: RpcMessage, Out: RpcMessage> Connector for IrohConnector<In, Out> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        let (request_ack_tx, request_ack_rx) = oneshot::channel();

        self.inner
            .requests_tx
            .send_async(request_ack_tx)
            .await
            .map_err(|_| quinn::ConnectionError::LocallyClosed)?;

        let (send, recv) = request_ack_rx
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
    type Item = Result<In, io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.project().0).poll_next(cx)
    }
}

/// Error for open. Currently just an anyhow::Error
pub type OpenBiError = anyhow::Error;

/// Error for accept. Currently just a quinn::ConnectionError
pub type AcceptError = quinn::ConnectionError;
