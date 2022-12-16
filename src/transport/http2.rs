//! http2 transport
//!
//! Note that we are using the framing from http2, so we have to make sure that
//! the parameters on both client and server side are big enough.
use std::{
    convert::Infallible, error, fmt, marker::PhantomData, net::SocketAddr, result, task::Poll,
};

use crate::RpcMessage;
use bytes::Bytes;
use flume::{r#async::RecvFut, Receiver, Sender};
use futures::{Future, FutureExt, StreamExt};
use hyper::{
    body::HttpBody,
    client::{connect::dns::GaiResolver, HttpConnector, ResponseFuture},
    server::conn::{AddrIncoming, AddrStream},
    service::{make_service_fn, service_fn},
    Body, Client, Request, Response, Server, StatusCode, Uri,
};
use pin_project::pin_project;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

// add a bit of fudge factor to the max frame size
const MAX_FRAME_SIZE: u32 = 1024 * 1024 + 1024;

macro_rules! log {
    ($($arg:tt)*) => {
        if false {
            println!($($arg)*);
        }
    }
}

// /// concatenate the given stream and result from the future
// ///
// /// Even if the future does not complete, the stream is nevertheless completed
// /// once the inner stream completes.
// #[pin_project]
// struct TakeUntilEither<S, F>(Option<(S, F)>);

// impl<S, F> TakeUntilEither<S, F>
// where
//     S: Stream + Unpin,
//     F: Future<Output = S::Item> + Unpin,
// {
//     fn new(stream: S, future: F) -> Self {
//         Self(Some((stream, future)))
//     }
// }

// impl<S, F> Stream for TakeUntilEither<S, F>
// where
//     S: Stream + Unpin,
//     F: Future<Output = S::Item> + Unpin,
// {
//     type Item = S::Item;

//     fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
//         match &mut self.0 {
//             Some((s, f)) => {
//                 match s.poll_next_unpin(cx) {
//                     Poll::Ready(Some(x)) => Poll::Ready(Some(x)),
//                     Poll::Ready(None) => {
//                         self.0.take();
//                         Poll::Ready(None)
//                     },
//                     Poll::Pending => match f.poll_unpin(cx) {
//                         Poll::Ready(x) => {
//                             self.0.take();
//                             Poll::Ready(Some(x))
//                         }
//                         Poll::Pending => Poll::Pending,
//                     },
//                 }
//             }
//             None => Poll::Ready(None),
//         }
//     }
// }

/// Client channel
pub struct ClientChannel<In: RpcMessage, Out: RpcMessage>(
    Client<HttpConnector<GaiResolver>, Body>,
    Uri,
    PhantomData<(In, Out)>,
);

impl<In: RpcMessage, Out: RpcMessage> ClientChannel<In, Out> {
    /// create a client given an uri
    pub fn new(uri: Uri) -> Self {
        let mut connector = HttpConnector::new();
        connector.set_nodelay(true);
        let client = Client::builder()
            .http2_only(true)
            .http2_initial_connection_window_size(Some(MAX_FRAME_SIZE))
            .http2_initial_stream_window_size(Some(MAX_FRAME_SIZE))
            .http2_max_frame_size(Some(MAX_FRAME_SIZE))
            .http2_max_send_buf_size(MAX_FRAME_SIZE as usize)
            .build(connector);
        Self(client, uri, PhantomData)
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ClientChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ClientChannel")
            .field(&self.0)
            .field(&self.1)
            .finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ClientChannel<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), self.1.clone(), PhantomData)
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
    channel: Receiver<(Receiver<In>, Sender<Out>)>,
    /// The sender to stop the server.
    ///
    /// We never send anything over this really, simply dropping it makes the receiver
    /// complete and will shut down the hyper server.
    stop_tx: mpsc::Sender<()>,
}

impl<In: RpcMessage, Out: RpcMessage> ServerChannel<In, Out> {
    /// Creates a server listening on the [`SocketAddr`].
    pub fn serve(addr: &SocketAddr) -> hyper::Result<Self> {
        let (accept_tx, accept_rx) = flume::bounded(32);

        // The hyper "MakeService" which is called for each connection that is made to the
        // server.  It creates another Service which handles a single request.
        let service = make_service_fn(move |socket: &AddrStream| {
            let remote_addr = socket.remote_addr();
            log!("connection from {:?}", remote_addr);

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

        let mut addr_incomping = AddrIncoming::bind(addr)?;
        addr_incomping.set_nodelay(true);
        let server = Server::builder(addr_incomping)
            .http2_only(true)
            .http2_initial_connection_window_size(Some(MAX_FRAME_SIZE))
            .http2_initial_stream_window_size(Some(MAX_FRAME_SIZE))
            .http2_max_frame_size(Some(MAX_FRAME_SIZE))
            .http2_max_send_buf_size(MAX_FRAME_SIZE as usize)
            .serve(service);

        let (stop_tx, mut stop_rx) = mpsc::channel::<()>(1);
        let server = server.with_graceful_shutdown(async move {
            // If the sender is dropped this will also gracefully terminate the server.
            stop_rx.recv().await;
        });
        tokio::spawn(server);

        Ok(Self {
            channel: accept_rx,
            stop_tx,
        })
    }

    /// Handles a single HTTP2 request.
    ///
    /// This creates the channels to communicate the (optionally streaming) request and
    /// response and sends them to the [`ServerChannel`].
    async fn handle_one_http2_request(
        req: Request<Body>,
        accept_tx: Sender<(Receiver<In>, Sender<Out>)>,
    ) -> Result<Response<Body>, String> {
        let (req_tx, req_rx) = flume::bounded::<In>(32);
        let (res_tx, res_rx) = flume::bounded::<Out>(32);
        accept_tx
            .send_async((req_rx, res_tx))
            .await
            .map_err(|_e| "unable to send")?;

        Self::spawn_recv_forwarder(req, req_tx);
        let body = res_rx.into_stream().map(|out| {
            let data = bincode::serialize(&out)?;
            // TODO: check size
            Ok::<Vec<u8>, bincode::Error>(data)
        });

        // Create a response with the response body channel as the response body
        let response = Response::builder()
            .status(StatusCode::OK)
            .body(Body::wrap_stream(body))
            .map_err(|_| "unable to set body")?;
        Ok(response)
    }

    /// Spawns a task which forwards requests from the network to a flume channel.
    ///
    /// This task will read frames from the network, decodes them using bincode and forward them
    /// to the flume channel used to receive a request stream.
    ///
    /// Could also have been implemented as a stream map.
    ///
    /// If there is a network error, decoding failure, the flume channel closes or the request
    /// stream is simply ended this task will terminate.  So it is fine to ignore the returned
    /// [`JoinHandle`].
    ///
    /// The HTTP2 request comes from *req* and the decoded version is sent to `req_tx`.
    fn spawn_recv_forwarder(req: Request<Body>, req_tx: Sender<In>) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut stream = req.into_body();

            // This assumes each chunk received corresponds to a single HTTP2 frame.
            while let Some(chunk) = stream.next().await {
                match chunk {
                    Ok(chunk) => match bincode::deserialize::<In>(chunk.as_ref()) {
                        Ok(msg) => {
                            log!("server got msg {msg:?}");
                            match req_tx.send_async(msg).await {
                                Ok(()) => {}
                                Err(_cause) => {
                                    // channel closed
                                    break;
                                }
                            }
                        }
                        Err(_cause) => {
                            // deserialize error
                            break;
                        }
                    },
                    Err(_cause) => {
                        // error from the network layer
                        break;
                    }
                }
            }
        })
    }
}

// This does not want or need RpcMessage to be clone but still want to clone the
// ServerChannel and it's containing channels itself.  The derive macro can't cope with this
// so this needs to be written by hand.
impl<In: RpcMessage, Out: RpcMessage> Clone for ServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self {
            channel: self.channel.clone(),
            stop_tx: self.stop_tx.clone(),
        }
    }
}

/// Receive stream for http2 channels.
///
/// This is just a mem channel that is fed from a task that reads from the hyper stream.
pub type RecvStream<M> = super::mem::RecvStream<M>;

/// Send sink for http2 channels.
///
/// This is just a mem channel that is read from a task that writes to the hyper stream.
pub type SendSink<M> = super::mem::SendSink<M>;

/// Send error for http2 channels.
///
/// The only thing that can go wrong is that the task that writes to the hyper stream has died.
pub type SendError = super::mem::SendError;

/// Receive error for http2 channels.
///
/// Currently there can be no error. When the hyper stream is closed or errors, the stream will
/// just terminate.
///
/// TODO: there should be a way to signal abnormal termination, so the two interaction patterns
/// that rely on updates from the client can distinguish between normal and abnormal termination.
///
/// You can obviously work around this by having a "finish" message in your application level protocol.
pub type RecvError = super::mem::RecvError;

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
#[pin_project]
pub struct OpenBiFuture<'a, In, Out>(
    Option<Result<(ResponseFuture, flume::Sender<Out>), OpenBiError>>,
    PhantomData<&'a In>,
);

impl<'a, In: RpcMessage, Out: RpcMessage> OpenBiFuture<'a, In, Out> {
    fn new(value: Result<(ResponseFuture, flume::Sender<Out>), OpenBiError>) -> Self {
        Self(Some(value), PhantomData)
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<super::mem::Socket<In, Out>, OpenBiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.0 {
            Some(Ok((fut, _))) => {
                match fut.poll_unpin(cx) {
                    Poll::Ready(Ok(mut res)) => {
                        log!("OpenBiFuture got response");
                        let (_, out_tx) = this.0.take().unwrap().unwrap();
                        let (in_tx, in_rx) = flume::bounded::<In>(32);
                        let task = async move {
                            while let Some(item) = res.body_mut().data().await {
                                match item {
                                    Ok(msg) => {
                                        match bincode::deserialize::<In>(msg.as_ref()) {
                                            Ok(msg) => {
                                                log!("OpenBiFuture got response message {:?}", msg);
                                                match in_tx.send_async(msg).await {
                                                    Ok(_) => {}
                                                    Err(_cause) => {
                                                        // todo: log warning!
                                                        break;
                                                    }
                                                }
                                            }
                                            Err(_cause) => {
                                                // todo: log error!
                                                break;
                                            }
                                        }
                                    }
                                    Err(_cause) => {
                                        // todo: log error!
                                        break;
                                    }
                                }
                            }
                        };
                        // todo: push the task into a channel to be handled. That way
                        // we can have backpressure on the number of tasks if we want!
                        tokio::spawn(task);

                        let out_tx = super::mem::SendSink(out_tx.into_sink());
                        let in_rx = super::mem::RecvStream(in_rx.into_stream());
                        Poll::Ready(Ok((out_tx, in_rx)))
                    }
                    Poll::Ready(Err(cause)) => {
                        log!("OpenBiFuture got error {}", cause);
                        this.0.take();
                        Poll::Ready(Err(OpenBiError::Hyper(cause)))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            Some(Err(_)) => {
                // this is guaranteed to work since we are in Some(Err(_))
                let err = this.0.take().unwrap().unwrap_err();
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
pub struct AcceptBiFuture<'a, In, Out>(
    Option<Result<RecvFut<'a, (Receiver<In>, Sender<Out>)>, AcceptBiError>>,
);

impl<'a, In: RpcMessage, Out: RpcMessage> AcceptBiFuture<'a, In, Out> {
    #[allow(clippy::type_complexity)]
    fn new(value: Result<RecvFut<'a, (Receiver<In>, Sender<Out>)>, AcceptBiError>) -> Self {
        Self(Some(value))
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for AcceptBiFuture<'a, In, Out> {
    type Output = result::Result<super::mem::Socket<In, Out>, AcceptBiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.0 {
            Some(Ok(fut)) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok((recv, send))) => Poll::Ready(Ok((
                    super::mem::SendSink(send.into_sink()),
                    super::mem::RecvStream(recv.into_stream()),
                ))),
                Poll::Ready(Err(_cause)) => {
                    this.0.take();
                    Poll::Ready(Err(AcceptBiError::RemoteDropped))
                }
                Poll::Pending => Poll::Pending,
            },
            Some(Err(_)) => {
                // this is guaranteed to work since we are in Some(Err(_))
                match this.0.take() {
                    Some(Err(err)) => Poll::Ready(Err(err)),
                    _ => unreachable!(),
                }
            }
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
        log!("open_bi {}", self.1);
        let (out_tx, out_rx) = flume::bounded::<Out>(32);
        let out_stream = futures::stream::unfold(out_rx, |out_rx| async move {
            match out_rx.recv_async().await {
                Ok(value) => Some((bincode::serialize(&value).map(Bytes::from), out_rx)),
                Err(_cause) => None,
            }
        });
        let req: Result<Request<Body>, OpenBiError> = Request::post(&self.1)
            .body(Body::wrap_stream(out_stream))
            .map_err(OpenBiError::HyperHttp);
        let res: Result<(ResponseFuture, flume::Sender<Out>), OpenBiError> =
            req.map(|req| (self.0.request(req), out_tx));
        OpenBiFuture::new(res)
    }
}

impl<In: RpcMessage, Out: RpcMessage> crate::ServerChannel<In, Out, ChannelTypes>
    for ServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture::new(Ok(self.channel.recv_async()))
    }
}
