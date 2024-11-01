//! http2 transport using [hyper]
//!
//! [hyper]: https://crates.io/crates/hyper/
use std::{
    convert::Infallible, error, fmt, io, marker::PhantomData, net::SocketAddr, pin::Pin, result,
    sync::Arc, task::Poll,
};

use crate::transport::{Connection, ConnectionErrors, LocalAddr, ServerEndpoint};
use crate::RpcMessage;
use bytes::Bytes;
use flume::{Receiver, Sender};
use futures_lite::{Stream, StreamExt};
use futures_sink::Sink;
use hyper::{
    client::{connect::Connect, HttpConnector, ResponseFuture},
    server::conn::{AddrIncoming, AddrStream},
    service::{make_service_fn, service_fn},
    Body, Client, Request, Response, Server, StatusCode, Uri,
};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, event, trace, Level};

use super::ConnectionCommon;

struct HyperConnectionInner {
    client: Box<dyn Requester>,
    config: Arc<ChannelConfig>,
    uri: Uri,
}

/// Hyper based connection to a server
pub struct HyperConnection<In: RpcMessage, Out: RpcMessage> {
    inner: Arc<HyperConnectionInner>,
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> Clone for HyperConnection<In, Out> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _p: PhantomData,
        }
    }
}

/// Trait so we don't have to drag around the hyper internals
trait Requester: Send + Sync + 'static {
    fn request(&self, req: Request<Body>) -> ResponseFuture;
}

impl<C: Connect + Clone + Send + Sync + 'static> Requester for Client<C, Body> {
    fn request(&self, req: Request<Body>) -> ResponseFuture {
        self.request(req)
    }
}

impl<In: RpcMessage, Out: RpcMessage> HyperConnection<In, Out> {
    /// create a client given an uri and the default configuration
    pub fn new(uri: Uri) -> Self {
        Self::with_config(uri, ChannelConfig::default())
    }

    /// create a client given an uri and a custom configuration
    pub fn with_config(uri: Uri, config: ChannelConfig) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        Self::with_connector(connector, uri, Arc::new(config))
    }

    /// create a client given an uri and a custom configuration
    pub fn with_connector<C: Connect + Clone + Send + Sync + 'static>(
        connector: C,
        uri: Uri,
        config: Arc<ChannelConfig>,
    ) -> Self {
        let client = Client::builder()
            .http2_only(true)
            .http2_initial_connection_window_size(Some(config.max_frame_size))
            .http2_initial_stream_window_size(Some(config.max_frame_size))
            .http2_max_frame_size(Some(config.max_frame_size))
            .http2_max_send_buf_size(config.max_frame_size.try_into().unwrap())
            .build(connector);
        Self {
            inner: Arc::new(HyperConnectionInner {
                client: Box::new(client),
                uri,
                config,
            }),
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for HyperConnection<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("uri", &self.inner.uri)
            .field("config", &self.inner.config)
            .finish()
    }
}

/// A flume sender and receiver tuple.
type InternalChannel<In> = (
    Receiver<result::Result<In, RecvError>>,
    Sender<io::Result<Bytes>>,
);

/// Error when setting a channel configuration
#[derive(Debug, Clone)]
pub enum ChannelConfigError {
    /// The maximum frame size is invalid
    InvalidMaxFrameSize(u32),
    /// The maximum payload size is invalid
    InvalidMaxPayloadSize(usize),
}

impl fmt::Display for ChannelConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl error::Error for ChannelConfigError {}

/// Channel configuration
///
/// These settings apply to both client and server channels.
#[derive(Debug, Clone)]
pub struct ChannelConfig {
    /// The maximum frame size to use.
    max_frame_size: u32,
    max_payload_size: usize,
}

impl ChannelConfig {
    /// Set the maximum frame size.
    pub fn max_frame_size(mut self, value: u32) -> result::Result<Self, ChannelConfigError> {
        if !(0x4000..=0xFFFFFF).contains(&value) {
            return Err(ChannelConfigError::InvalidMaxFrameSize(value));
        }
        self.max_frame_size = value;
        Ok(self)
    }

    /// Set the maximum payload size.
    pub fn max_payload_size(mut self, value: usize) -> result::Result<Self, ChannelConfigError> {
        if !(4096..1024 * 1024 * 16).contains(&value) {
            return Err(ChannelConfigError::InvalidMaxPayloadSize(value));
        }
        self.max_payload_size = value;
        Ok(self)
    }
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            max_frame_size: 0xFFFFFF,
            max_payload_size: 0xFFFFFF,
        }
    }
}

/// A server endpoint using a hyper server
///
/// Each request made by the any client connection this channel will yield a `(recv, send)`
/// pair which allows receiving the request and sending the response.  Both these are
/// channels themselves to support streaming requests and responses.
///
/// Creating this spawns a tokio task which runs the server, once dropped this task is shut
/// down: no new connections will be accepted and existing channels will stop.
#[derive(Debug)]
pub struct HyperServerEndpoint<In: RpcMessage, Out: RpcMessage> {
    /// The channel.
    channel: Receiver<InternalChannel<In>>,
    /// The configuration.
    config: Arc<ChannelConfig>,
    /// The sender to stop the server.
    ///
    /// We never send anything over this really, simply dropping it makes the receiver
    /// complete and will shut down the hyper server.
    stop_tx: mpsc::Sender<()>,
    /// The local address this server is bound to.
    ///
    /// This is useful when the listen address uses a random port, `:0`, to find out which
    /// port was bound by the kernel.
    local_addr: [LocalAddr; 1],
    /// Phantom data for service
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> HyperServerEndpoint<In, Out> {
    /// Creates a server listening on the [`SocketAddr`], with the default configuration.
    pub fn serve(addr: &SocketAddr) -> hyper::Result<Self> {
        Self::serve_with_config(addr, Default::default())
    }

    /// Creates a server listening on the [`SocketAddr`] with a custom configuration.
    pub fn serve_with_config(addr: &SocketAddr, config: ChannelConfig) -> hyper::Result<Self> {
        let (accept_tx, accept_rx) = flume::bounded(32);

        // The hyper "MakeService" which is called for each connection that is made to the
        // server.  It creates another Service which handles a single request.
        let service = make_service_fn(move |socket: &AddrStream| {
            let remote_addr = socket.remote_addr();
            event!(Level::TRACE, "Connection from {:?}", remote_addr);

            // Need a new accept_tx to move to the future on every call of this FnMut.
            let accept_tx = accept_tx.clone();
            async move {
                let one_req_service = service_fn(move |req: Request<Body>| {
                    // This closure is an FnMut as well, so clone accept_tx once more.
                    Self::handle_one_http2_request(req, accept_tx.clone())
                });
                Ok::<_, Infallible>(one_req_service)
            }
        });

        let mut incoming = AddrIncoming::bind(addr)?;
        incoming.set_nodelay(true);
        let server = Server::builder(incoming)
            .http2_only(true)
            .http2_initial_connection_window_size(Some(config.max_frame_size))
            .http2_initial_stream_window_size(Some(config.max_frame_size))
            .http2_max_frame_size(Some(config.max_frame_size))
            .http2_max_send_buf_size(config.max_frame_size.try_into().unwrap())
            .serve(service);
        let local_addr = server.local_addr();

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let server = server.with_graceful_shutdown(async move {
            // If the sender is dropped this will also gracefully terminate the server.
            stop_rx.recv().await;
        });
        tokio::spawn(server);

        Ok(Self {
            channel: accept_rx,
            config: Arc::new(config),
            stop_tx,
            local_addr: [LocalAddr::Socket(local_addr)],
            _p: PhantomData,
        })
    }

    /// Handles a single HTTP2 request.
    ///
    /// This creates the channels to communicate the (optionally streaming) request and
    /// response and sends them to the [`ServerChannel`].
    async fn handle_one_http2_request(
        req: Request<Body>,
        accept_tx: Sender<InternalChannel<In>>,
    ) -> Result<Response<Body>, String> {
        let (req_tx, req_rx) = flume::bounded::<result::Result<In, RecvError>>(32);
        let (res_tx, res_rx) = flume::bounded::<io::Result<Bytes>>(32);
        accept_tx
            .send_async((req_rx, res_tx))
            .await
            .map_err(|_e| "unable to send")?;

        spawn_recv_forwarder(req.into_body(), req_tx);
        // Create a response with the response body channel as the response body
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::wrap_stream(res_rx.into_stream()))
            .map_err(|_| "unable to set body")?;
        Ok(response)
    }
}

fn try_get_length_prefixed(buf: &[u8]) -> Option<&[u8]> {
    if buf.len() < 4 {
        return None;
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() < 4 + len {
        return None;
    }
    Some(&buf[4..4 + len])
}

/// Try forward all frames as deserialized messages from the buffer to the sender.
///
/// On success, returns the number of forwarded bytes.
/// On forward error, returns the unit error.
///
/// Deserialization errors don't cause an error, they will be sent.
/// On error the number of consumed bytes is not returned. There is nothing to do but
/// to stop the forwarder since there is nowhere to forward to anymore.
async fn try_forward_all<In: RpcMessage>(
    buffer: &[u8],
    req_tx: &Sender<Result<In, RecvError>>,
) -> result::Result<usize, ()> {
    let mut sent = 0;
    while let Some(msg) = try_get_length_prefixed(&buffer[sent..]) {
        sent += msg.len() + 4;
        let item = bincode::deserialize::<In>(msg).map_err(RecvError::DeserializeError);
        if let Err(_cause) = req_tx.send_async(item).await {
            // The receiver is gone, so we can't send any more data.
            //
            // This is a normal way for an interaction to end, when the server side is done processing
            // the request and drops the receiver.
            //
            // don't log the cause. It does not contain any useful information.
            trace!("Flume receiver dropped");
            return Err(());
        }
    }
    Ok(sent)
}

/// Spawns a task which forwards requests from the network to a flume channel.
///
/// This task will read chunks from the network, split them into length prefixed
/// frames, deserialize those frames, and send the result to the flume channel.
///
/// If there is a network error or the flume channel closes or the request
/// stream is simply ended this task will terminate.
///
/// So it is fine to ignore the returned [`JoinHandle`].
///
/// The HTTP2 request comes from *req* and the data is sent to `req_tx`.
fn spawn_recv_forwarder<In: RpcMessage>(
    req: Body,
    req_tx: Sender<result::Result<In, RecvError>>,
) -> JoinHandle<result::Result<(), ()>> {
    tokio::spawn(async move {
        let mut stream = req;
        let mut buf = Vec::new();

        while let Some(chunk) = stream.next().await {
            match chunk.as_ref() {
                Ok(chunk) => {
                    event!(Level::TRACE, "Server got {} bytes", chunk.len());
                    if buf.is_empty() {
                        // try to forward directly from buffer
                        let sent = try_forward_all(chunk, &req_tx).await?;
                        // add just the rest, if any
                        buf.extend_from_slice(&chunk[sent..]);
                    } else {
                        // no choice but to add it all
                        buf.extend_from_slice(chunk);
                    }
                }
                Err(cause) => {
                    // Indicates that the connection has been closed on the client side.
                    // This is a normal occurrence, e.g. when the client has raced the RPC
                    // call with something else and has droppped the future.
                    debug!("Network error: {}", cause);
                    break;
                }
            };
            let sent = try_forward_all(&buf, &req_tx).await?;
            // remove the forwarded bytes.
            // Frequently this will be the entire buffer, so no memcpy but just set the size to 0
            buf.drain(..sent);
        }
        Ok(())
    })
}

// This does not want or need RpcMessage to be clone but still want to clone the
// ServerChannel and it's containing channels itself.  The derive macro can't cope with this
// so this needs to be written by hand.
impl<In: RpcMessage, Out: RpcMessage> Clone for HyperServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            stop_tx: self.stop_tx.clone(),
            local_addr: self.local_addr.clone(),
            config: self.config.clone(),
            _p: PhantomData,
        }
    }
}

/// Receive stream for hyper channels.
///
/// This is a newtype wrapper around a [`flume::async::RecvStream`] of deserialized
/// messages.
pub struct RecvStream<Res: RpcMessage> {
    recv: flume::r#async::RecvStream<'static, result::Result<Res, RecvError>>,
}

impl<Res: RpcMessage> RecvStream<Res> {
    /// Creates a new [`RecvStream`] from a [`flume::Receiver`].
    pub fn new(recv: flume::Receiver<result::Result<Res, RecvError>>) -> Self {
        Self {
            recv: recv.into_stream(),
        }
    }

    // we can not write into_inner, since all we got is a stream of already
    // framed and deserialize messages. Might want to change that...
}

impl<In: RpcMessage> Clone for RecvStream<In> {
    fn clone(&self) -> Self {
        Self {
            recv: self.recv.clone(),
        }
    }
}

impl<Res: RpcMessage> Stream for RecvStream<Res> {
    type Item = Result<Res, RecvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.recv).poll_next(cx)
    }
}

/// Send sink for hyper channels
pub struct SendSink<Out: RpcMessage> {
    sink: flume::r#async::SendSink<'static, io::Result<Bytes>>,
    config: Arc<ChannelConfig>,
    _p: PhantomData<Out>,
}

impl<Out: RpcMessage> SendSink<Out> {
    fn new(sender: flume::Sender<io::Result<Bytes>>, config: Arc<ChannelConfig>) -> Self {
        Self {
            sink: sender.into_sink(),
            config,
            _p: PhantomData,
        }
    }
    fn serialize(&self, item: Out) -> Result<Bytes, SendError> {
        let mut data = Vec::with_capacity(1024);
        data.extend_from_slice(&[0u8; 4]);
        bincode::serialize_into(&mut data, &item).map_err(SendError::SerializeError)?;
        let len = data.len() - 4;
        if len > self.config.max_payload_size {
            return Err(SendError::SizeError(len));
        }
        let len: u32 = len.try_into().expect("max_payload_size fits into u32");
        data[0..4].copy_from_slice(&len.to_be_bytes());
        Ok(data.into())
    }

    /// Consumes the [`SendSink`] and returns the underlying [`flume::async::SendSink`].
    ///
    /// This is useful if you want to send raw [bytes::Bytes] without framing
    /// directly to the channel.
    pub fn into_inner(self) -> flume::r#async::SendSink<'static, io::Result<Bytes>> {
        self.sink
    }
}

impl<Out: RpcMessage> Sink<Out> for SendSink<Out> {
    type Error = SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_ready(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn start_send(mut self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        // figure out what to send and what to return
        let (send, res) = match self.serialize(item) {
            Ok(data) => (Ok(data), Ok(())),
            Err(cause) => (
                Err(io::Error::new(io::ErrorKind::Other, cause.to_string())),
                Err(cause),
            ),
        };
        // attempt sending
        Pin::new(&mut self.sink)
            .start_send(send)
            .map_err(|_| SendError::ReceiverDropped)?;
        res
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_flush(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.sink)
            .poll_close(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }
}

/// Send error for hyper channels.
#[derive(Debug)]
pub enum SendError {
    /// Error when bincode serializing the message.
    SerializeError(bincode::Error),
    /// The message is too large to be sent.
    SizeError(usize),
    /// The connection has been closed.
    ReceiverDropped,
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl error::Error for SendError {}

/// Receive error for hyper channels.
#[derive(Debug)]
pub enum RecvError {
    /// Error when bincode deserializing the message.
    DeserializeError(bincode::Error),
    /// Hyper network error.
    NetworkError(hyper::Error),
}

impl fmt::Display for RecvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self, f)
    }
}

impl error::Error for RecvError {}

/// OpenBiError for hyper channels.
#[derive(Debug)]
pub enum OpenBiError {
    /// Hyper http error
    HyperHttp(hyper::http::Error),
    /// Generic hyper error
    Hyper(hyper::Error),
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl fmt::Display for OpenBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for OpenBiError {}

/// AcceptBiError for hyper channels.
///
/// There is not much that can go wrong with hyper channels.
#[derive(Debug)]
pub enum AcceptBiError {
    /// Hyper error
    Hyper(hyper::http::Error),
    /// The remote side of the channel was dropped
    RemoteDropped,
}

impl fmt::Display for AcceptBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for AcceptBiError {}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for HyperConnection<In, Out> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = OpenBiError;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon for HyperConnection<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = self::RecvStream<In>;
    type SendSink = self::SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> Connection for HyperConnection<In, Out> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        let (out_tx, out_rx) = flume::bounded::<io::Result<Bytes>>(32);
        let req: Request<Body> = Request::post(&self.inner.uri)
            .body(Body::wrap_stream(out_rx.into_stream()))
            .map_err(OpenBiError::HyperHttp)?;
        let res = self
            .inner
            .client
            .request(req)
            .await
            .map_err(OpenBiError::Hyper)?;
        let (in_tx, in_rx) = flume::bounded::<result::Result<In, RecvError>>(32);
        spawn_recv_forwarder(res.into_body(), in_tx);

        let out_tx = self::SendSink::new(out_tx, self.inner.config.clone());
        let in_rx = self::RecvStream::new(in_rx);
        Ok((out_tx, in_rx))
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for HyperServerEndpoint<In, Out> {
    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenError = AcceptBiError;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon for HyperServerEndpoint<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = self::RecvStream<In>;
    type SendSink = self::SendSink<Out>;
}

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint for HyperServerEndpoint<In, Out> {
    fn local_addr(&self) -> &[LocalAddr] {
        &self.local_addr
    }

    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), AcceptBiError> {
        let (recv, send) = self
            .channel
            .recv_async()
            .await
            .map_err(|_| AcceptBiError::RemoteDropped)?;
        Ok((
            SendSink::new(send, self.config.clone()),
            RecvStream::new(recv),
        ))
    }
}
