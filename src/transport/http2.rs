//! http2 transport
//!
//! Note that we are using the framing from http2, so we have to make sure that
//! the parameters on both client and server side are big enough.
use std::{
    convert::Infallible, error, fmt, io, marker::PhantomData, net::SocketAddr, pin::Pin, result,
    sync::Arc, task::Poll,
};

use crate::RpcMessage;
use bytes::Bytes;
use flume::{r#async::RecvFut, Receiver, Sender};
use futures::{Future, FutureExt, Sink, SinkExt, StreamExt};
use hyper::{
    client::{connect::Connect, HttpConnector, ResponseFuture},
    server::conn::AddrIncoming,
    service::{make_service_fn, service_fn},
    Body, Client, Request, Response, Server, StatusCode, Uri,
};
use pin_project::pin_project;
use tokio::task::JoinHandle;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tracing::{error, event, Level};

/// Client channel
///
/// TODO: this calls for ClientChannelInner...
pub struct ClientChannel<In: RpcMessage, Out: RpcMessage> {
    client: Arc<dyn Requester>,
    config: Arc<ChannelConfig>,
    uri: Uri,
    _p: PhantomData<(In, Out)>,
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

impl<In: RpcMessage, Out: RpcMessage> ClientChannel<In, Out> {
    /// create a client given an uri and the default configuration
    pub fn new(uri: Uri) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        Self::new_with_connector(connector, uri, Arc::new(ChannelConfig::default()))
    }

    /// create a client given an uri and a custom configuration
    pub fn new_with_connector<C: Connect + Clone + Send + Sync + 'static>(
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
            client: Arc::new(client),
            uri,
            config,
            _p: PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientChannel")
            .field("uri", &self.uri)
            .field("config", &self.config)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            client: self.client.clone(),
            uri: self.uri.clone(),
            config: self.config.clone(),
            _p: PhantomData,
        }
    }
}

type Socket<In, Out> = (self::SendSink<Out>, self::RecvStream<In>);

type InternalChannel = (Receiver<hyper::Result<Bytes>>, Sender<io::Result<Bytes>>);
/// Error when setting a channel configuration
#[derive(Debug, Clone)]
pub enum ChannelConfigError {
    /// The maximum frame size is invalid
    InvalidMaxFrameSize(u32),
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
}

impl Default for ChannelConfig {
    fn default() -> Self {
        Self {
            max_frame_size: 0xFFFFFF,
        }
    }
}

/// A server-side channel using a hyper connection
///
/// Each request made by the any client connection this channel will yield a `(recv, send)`
/// pair which allows receiving the request and sending the response.  Both these are
/// channels themselves to support streaming requests and responses.
///
/// Creating this spawns a tokio task which runs the server, once dropped this task is shut
/// down: no new connections will be accepted and existing channels will stop.
#[derive(Debug)]
pub struct ServerChannel<In: RpcMessage, Out: RpcMessage> {
    /// The channel.
    channel: Receiver<InternalChannel>,
    /// The configuration.
    config: Arc<ChannelConfig>,
    /// The sender to stop the server.
    ///
    /// We never send anything over this really, simply dropping it makes the receiver
    /// complete and will shut down the hyper server.
    stop_tx: mpsc::Sender<()>,
    /// Phantom data for in and out
    _p: PhantomData<(In, Out)>,
}

impl<In: RpcMessage, Out: RpcMessage> ServerChannel<In, Out> {
    /// Creates a server listening on the [`SocketAddr`], with the default configuration.
    pub fn serve(addr: &SocketAddr) -> hyper::Result<Self> {
        let mut addr_incoming = AddrIncoming::bind(addr)?;
        addr_incoming.set_nodelay(true);
        Self::serve0(addr_incoming, Default::default())
    }

    /// Creates a server listener given an arbitrary incoming and a custom configuration.
    pub fn serve_with_incoming<I>(incoming: I, config: Arc<ChannelConfig>) -> hyper::Result<Self>
    where
        I: hyper::server::accept::Accept + Send + 'static,
        I::Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        I::Error: Into<Box<dyn error::Error + Send + Sync>>,
    {
        Self::serve0(incoming, config)
    }

    fn serve0<I>(incoming: I, config: Arc<ChannelConfig>) -> hyper::Result<Self>
    where
        I: hyper::server::accept::Accept + Send + 'static,
        I::Conn: AsyncRead + AsyncWrite + Unpin + Send + 'static,
        I::Error: Into<Box<dyn error::Error + Send + Sync>>,
    {
        let (accept_tx, accept_rx) = flume::bounded(32);

        // The hyper "MakeService" which is called for each connection that is made to the
        // server.  It creates another Service which handles a single request.
        let service = make_service_fn(move |_socket: &I::Conn| {
            // TODO: log remote address
            // let remote_addr = socket.remote_addr();
            // event!(Level::TRACE, "Connection from {:?}", remote_addr);

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

        let server = Server::builder(incoming)
            .http2_only(true)
            .http2_initial_connection_window_size(Some(config.max_frame_size))
            .http2_initial_stream_window_size(Some(config.max_frame_size))
            .http2_max_frame_size(Some(config.max_frame_size))
            .http2_max_send_buf_size(config.max_frame_size.try_into().unwrap())
            .serve(service);

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let server = server.with_graceful_shutdown(async move {
            // If the sender is dropped this will also gracefully terminate the server.
            stop_rx.recv().await;
        });
        tokio::spawn(server);

        Ok(Self {
            channel: accept_rx,
            config,
            stop_tx,
            _p: PhantomData,
        })
    }

    /// Handles a single HTTP2 request.
    ///
    /// This creates the channels to communicate the (optionally streaming) request and
    /// response and sends them to the [`ServerChannel`].
    async fn handle_one_http2_request(
        req: Request<Body>,
        accept_tx: Sender<InternalChannel>,
    ) -> Result<Response<Body>, String> {
        let (req_tx, req_rx) = flume::bounded::<hyper::Result<Bytes>>(32);
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

/// Spawns a task which forwards requests from the network to a flume channel.
///
/// This task will read frames from the network, filter out empty frames, and
/// forward non-empty frames to the flume channel. It will send the first error,
/// but will then terminate.
///
/// If there is a network error or the flume channel closes or the request
/// stream is simply ended this task will terminate.
///
/// So it is fine to ignore the returned [`JoinHandle`].
///
/// The HTTP2 request comes from *req* and the data is sent to `req_tx`.
fn spawn_recv_forwarder(req: Body, req_tx: Sender<hyper::Result<Bytes>>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut stream = req;

        // This assumes each chunk received corresponds to a single HTTP2 frame.
        while let Some(chunk) = stream.next().await {
            let exit = match chunk.as_ref() {
                Ok(chunk) => {
                    if chunk.is_empty() {
                        // Ignore empty chunks. we won't send empty chunks.
                        continue;
                    }
                    event!(Level::TRACE, "Server got msg: {} bytes", chunk.len());
                    false
                }
                Err(cause) => {
                    error!("Network error: {}", cause);
                    true
                }
            };
            if let Err(_cause) = req_tx.send_async(chunk).await {
                // don't log the cause. It does not contain any useful information.
                error!("Flume receiver dropped");
                break;
            }
            if exit {
                break;
            }
        }
    })
}

// This does not want or need RpcMessage to be clone but still want to clone the
// ServerChannel and it's containing channels itself.  The derive macro can't cope with this
// so this needs to be written by hand.
impl<In: RpcMessage, Out: RpcMessage> Clone for ServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            stop_tx: self.stop_tx.clone(),
            config: self.config.clone(),
            _p: PhantomData,
        }
    }
}

/// Receive stream for http2 channels.
pub struct RecvStream<Res: RpcMessage>(
    flume::r#async::RecvStream<'static, hyper::Result<Bytes>>,
    PhantomData<Res>,
);

impl<Res: RpcMessage> RecvStream<Res> {
    /// Creates a new [`RecvStream`] from a [`flume::Receiver`].
    pub fn new(recv: flume::Receiver<hyper::Result<Bytes>>) -> Self {
        Self(recv.into_stream(), PhantomData)
    }
}

impl<In: RpcMessage> Clone for RecvStream<In> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

impl<Res: RpcMessage> futures::Stream for RecvStream<Res> {
    type Item = Result<Res, RecvError>;

    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        match self.0.poll_next_unpin(cx) {
            Poll::Ready(Some(Ok(item))) => match bincode::deserialize::<Res>(item.as_ref()) {
                Ok(msg) => Poll::Ready(Some(Ok(msg))),
                Err(cause) => {
                    println!("{} {}", item.len(), cause);
                    Poll::Ready(Some(Err(RecvError::DeserializeError(cause))))
                }
            },
            Poll::Ready(Some(Err(cause))) => Poll::Ready(Some(Err(RecvError::NetworkError(cause)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// SendSink for http2 channels
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
        let data = bincode::serialize(&item).map_err(SendError::SerializeError)?;
        let max_payload_size = self.config.max_frame_size as usize - 1024;
        if data.is_empty() || data.len() > max_payload_size {
            return Err(SendError::SizeError(data.len()));
        }
        Ok(data.into())
    }
}

impl<Out: RpcMessage> Sink<Out> for SendSink<Out> {
    type Error = SendError;

    fn poll_ready(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.sink
            .poll_ready_unpin(cx)
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
        self.sink
            .start_send_unpin(send)
            .map_err(|_| SendError::ReceiverDropped)?;
        res
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.sink
            .poll_flush_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }

    fn poll_close(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.sink
            .poll_close_unpin(cx)
            .map_err(|_| SendError::ReceiverDropped)
    }
}

/// Send error for http2 channels.
///
/// The only thing that can go wrong is that the task that writes to the hyper stream has died.
#[derive(Debug)]
pub enum SendError {
    /// Error when bincode serializing the message.
    SerializeError(bincode::Error),
    /// The message is too large to be sent, or zero size.
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

/// Receive error for http2 channels.
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

/// Http2 channel types
#[derive(Debug, Clone)]
pub struct ChannelTypes;

impl crate::ChannelTypes for ChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type ClientChannel<In: RpcMessage, Out: RpcMessage> = self::ClientChannel<In, Out>;

    type ServerChannel<In: RpcMessage, Out: RpcMessage> = self::ServerChannel<In, Out>;
}

/// OpenBiError for mem channels.
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

/// Future returned by `Channel::open_bi`.
#[allow(clippy::type_complexity)]
#[pin_project]
pub struct OpenBiFuture<'a, In, Out> {
    chan: Option<
        Result<
            (
                ResponseFuture,
                flume::Sender<io::Result<Bytes>>,
                Arc<ChannelConfig>,
            ),
            OpenBiError,
        >,
    >,
    _p: PhantomData<&'a (In, Out)>,
}

#[allow(clippy::type_complexity)]
impl<'a, In: RpcMessage, Out: RpcMessage> OpenBiFuture<'a, In, Out> {
    fn new(
        value: Result<
            (
                ResponseFuture,
                flume::Sender<io::Result<Bytes>>,
                Arc<ChannelConfig>,
            ),
            OpenBiError,
        >,
    ) -> Self {
        Self {
            chan: Some(value),
            _p: PhantomData,
        }
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, OpenBiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.chan {
            Some(Ok((fut, _, _))) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok(res)) => {
                    event!(Level::TRACE, "OpenBiFuture got response");
                    let (_, out_tx, config) = this.chan.take().unwrap().unwrap();
                    let (in_tx, in_rx) = flume::bounded::<hyper::Result<Bytes>>(32);
                    spawn_recv_forwarder(res.into_body(), in_tx);

                    let out_tx = self::SendSink::new(out_tx, config);
                    let in_rx = self::RecvStream::new(in_rx);
                    Poll::Ready(Ok((out_tx, in_rx)))
                }
                Poll::Ready(Err(cause)) => {
                    event!(Level::TRACE, "OpenBiFuture got error {}", cause);
                    this.chan.take();
                    Poll::Ready(Err(OpenBiError::Hyper(cause)))
                }
                Poll::Pending => Poll::Pending,
            },
            Some(Err(_)) => {
                // this is guaranteed to work since we are in Some(Err(_))
                let err = this.chan.take().unwrap().unwrap_err();
                Poll::Ready(Err(err))
            }
            None => {
                // return pending once the option is none, as per the contract
                // for a fused future
                Poll::Pending
            }
        }
    }
}

/// AcceptBiError for mem channels.
///
/// There is not much that can go wrong with mem channels.
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

/// Future returned by `Channel::accept_bi`.
#[allow(clippy::type_complexity)]
#[pin_project]
pub struct AcceptBiFuture<'a, In, Out> {
    chan: Option<(
        RecvFut<'a, (Receiver<hyper::Result<Bytes>>, Sender<io::Result<Bytes>>)>,
        Arc<ChannelConfig>,
    )>,
    _p: PhantomData<(In, Out)>,
}

impl<'a, In: RpcMessage, Out: RpcMessage> AcceptBiFuture<'a, In, Out> {
    #[allow(clippy::type_complexity)]
    fn new(
        fut: RecvFut<'a, (Receiver<hyper::Result<Bytes>>, Sender<io::Result<Bytes>>)>,
        config: Arc<ChannelConfig>,
    ) -> Self {
        Self {
            chan: Some((fut, config)),
            _p: PhantomData,
        }
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<self::Socket<In, Out>, AcceptBiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.chan {
            Some((fut, _)) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok((recv, send))) => {
                    let (_, config) = this.chan.take().unwrap();
                    Poll::Ready(Ok((
                        self::SendSink::new(send, config),
                        self::RecvStream::new(recv),
                    )))
                }
                Poll::Ready(Err(_cause)) => {
                    this.chan.take();
                    Poll::Ready(Err(AcceptBiError::RemoteDropped))
                }
                Poll::Pending => Poll::Pending,
            },
            None => {
                // return pending once the option is none, as per the contract
                // for a fused future
                Poll::Pending
            }
        }
    }
}

impl<In, Out> crate::ClientChannel<In, Out, ChannelTypes> for ClientChannel<In, Out>
where
    In: RpcMessage,
    Out: RpcMessage,
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        event!(Level::TRACE, "open_bi {}", self.uri);
        let (out_tx, out_rx) = flume::bounded::<io::Result<Bytes>>(32);
        let req: Result<Request<Body>, OpenBiError> = Request::post(&self.uri)
            .body(Body::wrap_stream(out_rx.into_stream()))
            .map_err(OpenBiError::HyperHttp);
        let res = req.map(|req| (self.client.request(req), out_tx, self.config.clone()));
        OpenBiFuture::new(res)
    }
}

impl<In: RpcMessage, Out: RpcMessage> crate::ServerChannel<In, Out, ChannelTypes>
    for ServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture::new(self.channel.recv_async(), self.config.clone())
    }
}
