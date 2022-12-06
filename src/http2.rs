//! http2 transport
//!
//! Note that we are using the framing from http2, so we have to make sure that
//! the parameters on both client and server side are big enough.
use std::{
    convert::Infallible, error, fmt, io, marker::PhantomData, net::SocketAddr, result, task::Poll,
};

use crate::{ChannelTypes, RpcMessage};
use bytes::Bytes;
use flume::{r#async::RecvFut, Receiver, Sender};
use futures::{future, never::Never, Future, FutureExt, StreamExt};
use hyper::{
    body::HttpBody,
    client::{connect::dns::GaiResolver, HttpConnector, ResponseFuture},
    server::conn::AddrStream,
    service::{make_service_fn, service_fn},
    Body, Client, Request, Response, Server, StatusCode, Uri,
};
use pin_project::pin_project;

const MAX_FRAME_SIZE: u32 = 1024 * 1024 * 2;

macro_rules! log {
    ($($arg:tt)*) => {
        // println!($($arg)*);
        let _ = format!($($arg)*);
    }
}

/// A channel using a hyper connection
pub enum Channel<In: RpcMessage, Out: RpcMessage> {
    Client(Client<HttpConnector<GaiResolver>, Body>, Uri),
    Server(flume::Receiver<(flume::Receiver<In>, flume::Sender<Out>)>),
}

impl<In: RpcMessage, Out: RpcMessage> Channel<In, Out> {
    pub fn client(uri: Uri) -> Self {
        let client = Client::builder()
            .http2_only(true)
            // .http2_initial_connection_window_size(Some(1024 * 1024 *2))
            // .http2_initial_stream_window_size(Some(1024 * 1024 *2))
            .http2_max_frame_size(Some(MAX_FRAME_SIZE))
            .http2_max_send_buf_size(MAX_FRAME_SIZE as usize)
            .build_http();
        Self::Client(client, uri)
    }

    pub fn server(
        addr: &SocketAddr,
    ) -> (Self, impl Future<Output = result::Result<(), hyper::Error>>) {
        let (accept_tx, accept_rx) = flume::bounded(1);
        let server_fn = move |req: Request<Body>| {
            async move {
                // Create Flume channels for the request body and response body
                let (req_tx, req_rx) = flume::bounded::<In>(1);
                let (res_tx, res_rx) = flume::bounded::<Out>(1);
                accept_tx
                    .send_async((req_rx, res_tx))
                    .await
                    .map_err(|_e| "unable to send")?;
                // task that reads request and deserializes it
                let forwarder = async move {
                    let mut stream = req.into_body();
                    while let Some(chunk) = stream.next().await {
                        match chunk {
                            Ok(chunk) => match bincode::deserialize::<In>(chunk.as_ref()) {
                                Ok(msg) => {
                                    log!("server got msg {:?}", msg);
                                    match req_tx.send_async(msg).await {
                                        Ok(()) => {}
                                        Err(_cause) => {
                                            break;
                                        }
                                    }
                                }
                                Err(_cause) => {
                                    break;
                                }
                            },
                            Err(_cause) => {
                                break;
                            }
                        }
                    }
                    future::pending::<Never>().await
                };
                // todo: can I piggyback on the polling of the stream below instead of having
                // to spawn a task?
                tokio::spawn(forwarder);
                let body = res_rx.into_stream().map(|out| {
                    let data = bincode::serialize(&out)?;
                    // todo: check size
                    Ok::<Vec<u8>, bincode::Error>(data)
                });
                // Create a response with the response body channel as the response body
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::wrap_stream(body))
                    .map_err(|_e| "unable to set body")?;

                Ok::<Response<Body>, &str>(response)
            }
        };
        // todo: can this stack of weird stuff be simplified?
        let service = make_service_fn(move |socket: &AddrStream| {
            let remote_addr = socket.remote_addr();
            log!("connection from {:?}", remote_addr);
            let server_fn = server_fn.clone();
            async move {
                let server_fn = server_fn.clone();
                Ok::<_, Infallible>(service_fn(move |req: Request<Body>| {
                    let server_fn = server_fn.clone();
                    server_fn(req)
                }))
            }
        });
        let server = Server::bind(addr)
            .http2_only(true)
            .http2_max_frame_size(Some(MAX_FRAME_SIZE))
            .http2_max_send_buf_size(MAX_FRAME_SIZE as usize)
            .serve(service);
        (Self::Server(accept_rx), server)
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for Channel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Client(arg0, arg1) => f.debug_tuple("Client").field(arg0).field(arg1).finish(),
            Self::Server(arg0) => f.debug_tuple("Server").field(arg0).finish(),
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for Channel<In, Out> {
    fn clone(&self) -> Self {
        match self {
            Self::Client(arg0, arg1) => Self::Client(arg0.clone(), arg1.clone()),
            Self::Server(arg0) => Self::Server(arg0.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Http2ChannelTypes;

impl ChannelTypes for Http2ChannelTypes {
    type SendSink<M: RpcMessage> = crate::mem::SendSink<M>;

    type RecvStream<M: RpcMessage> = crate::mem::RecvStream<M>;

    type SendError = crate::mem::SendError;

    type RecvError = crate::mem::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type CreateChannelError = io::Error;

    type Channel<In: RpcMessage, Out: RpcMessage> = self::Channel<In, Out>;
}

/// OpenBiError for mem channels.
#[derive(Debug)]
pub enum OpenBiError {
    HyperHttp(hyper::http::Error),
    Hyper(hyper::Error),
    /// The remote side of the channel was dropped
    RemoteDropped,
    Server,
}

impl fmt::Display for OpenBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl std::error::Error for OpenBiError {}

#[pin_project]
pub struct OpenBiFuture<'a, In, Out>(
    Option<Result<(ResponseFuture, flume::Sender<Out>), OpenBiError>>,
    PhantomData<&'a (In, Out)>,
);

impl<'a, In: RpcMessage, Out: RpcMessage> OpenBiFuture<'a, In, Out> {
    fn new(value: Result<(ResponseFuture, flume::Sender<Out>), OpenBiError>) -> Self {
        Self(Some(value), PhantomData)
    }
}

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<'a, In, Out> {
    type Output = result::Result<crate::mem::Socket<In, Out>, OpenBiError>;

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
                        let (in_tx, in_rx) = flume::bounded::<In>(1);
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

                        let out_tx = crate::mem::SendSink(out_tx.into_sink());
                        let in_rx = crate::mem::RecvStream(in_rx.into_stream());
                        Poll::Ready(Ok((out_tx, in_rx)))
                    }
                    Poll::Ready(Err(cause)) => {
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
    /// Tried to accept on client side
    Client,
}

impl fmt::Display for AcceptBiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl error::Error for AcceptBiError {}

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
    type Output = result::Result<crate::mem::Socket<In, Out>, AcceptBiError>;

    fn poll(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let this = self.project();
        match this.0 {
            Some(Ok(fut)) => match fut.poll_unpin(cx) {
                Poll::Ready(Ok((recv, send))) => Poll::Ready(Ok((
                    crate::mem::SendSink(send.into_sink()),
                    crate::mem::RecvStream(recv.into_stream()),
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

impl<In: RpcMessage, Out: RpcMessage> crate::Channel<In, Out, Http2ChannelTypes>
    for Channel<In, Out>
{
    fn open_bi(&self) -> OpenBiFuture<'_, In, Out> {
        log!("open_bi");
        match self {
            Channel::Client(client, url) => {
                let (out_tx, out_rx) = flume::bounded::<Out>(1);
                let out_stream = futures::stream::unfold(out_rx, |out_rx| async move {
                    match out_rx.recv_async().await {
                        Ok(value) => Some((bincode::serialize(&value).map(Bytes::from), out_rx)),
                        Err(_cause) => None,
                    }
                });
                let req: Result<Request<Body>, OpenBiError> = Request::post(url)
                    .body(Body::wrap_stream(out_stream))
                    .map_err(OpenBiError::HyperHttp);
                let res: Result<(ResponseFuture, flume::Sender<Out>), OpenBiError> =
                    req.map(|req| (client.request(req), out_tx));
                OpenBiFuture::new(res)
            }
            Channel::Server(_) => OpenBiFuture::new(Err(OpenBiError::Server)),
        }
    }

    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        match self {
            Channel::Client(_, _) => AcceptBiFuture::new(Err(AcceptBiError::Client)),
            Channel::Server(server) => AcceptBiFuture::new(Ok(server.recv_async())),
        }
    }
}
