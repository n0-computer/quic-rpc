//! http2 transport
//!
//! Note that we are using the framing from http2, so we have to make sure that
//! the parameters on both client and server side are big enough.
use std::{
    convert::Infallible, error, fmt, marker::PhantomData, net::SocketAddr, result, task::Poll,
};

use crate::{ChannelTypes, RpcMessage};
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

/// A channel using a hyper connection
pub struct ServerChannel<In: RpcMessage, Out: RpcMessage>(
    flume::Receiver<(flume::Receiver<In>, flume::Sender<Out>)>,
);

impl<In: RpcMessage, Out: RpcMessage> ServerChannel<In, Out> {
    /// create a server given a socket addr
    pub fn new(
        addr: &SocketAddr,
    ) -> hyper::Result<(Self, impl Future<Output = result::Result<(), hyper::Error>>)> {
        let (accept_tx, accept_rx) = flume::bounded(32);
        let server_fn = move |req: Request<Body>| {
            async move {
                // Create Flume channels for the request body and response body
                let (req_tx, req_rx) = flume::bounded::<In>(32);
                let (res_tx, res_rx) = flume::bounded::<Out>(32);
                accept_tx
                    .send_async((req_rx, res_tx))
                    .await
                    .map_err(|_e| "unable to send")?;
                // task that reads requests, deserializes them, and forwards them to the flume channel
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
                };
                let body = res_rx.into_stream().map(|out| {
                    let data = bincode::serialize(&out)?;
                    // todo: check size
                    Ok::<Vec<u8>, bincode::Error>(data)
                });
                tokio::spawn(forwarder);
                // let body = TakeUntilEither::new(body, forwarder);
                // Create a response with the response body channel as the response body
                let response = Response::builder()
                    .status(StatusCode::OK)
                    .body(Body::wrap_stream(body))
                    .map_err(|_e| "unable to set body")?;

                Ok::<Response<Body>, String>(response)
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
        let mut addr_incomping = AddrIncoming::bind(addr)?;
        addr_incomping.set_nodelay(true);
        let server = Server::builder(addr_incomping)
            .http2_only(true)
            .http2_initial_connection_window_size(Some(MAX_FRAME_SIZE))
            .http2_initial_stream_window_size(Some(MAX_FRAME_SIZE))
            .http2_max_frame_size(Some(MAX_FRAME_SIZE))
            .http2_max_send_buf_size(MAX_FRAME_SIZE as usize)
            .serve(service);
        Ok((Self(accept_rx), server))
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for ServerChannel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("ServerChannel").field(&self.0).finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for ServerChannel<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

/// todo
pub type RecvStream<M> = crate::mem::RecvStream<M>;

/// todo
pub type SendSink<M> = crate::mem::SendSink<M>;

/// todo
pub type SendError = crate::mem::SendError;

/// todo
pub type RecvError = crate::mem::RecvError;

/// todo
pub type CreateChannelError = hyper::Error;

/// Http2 channel types
#[derive(Debug, Clone)]
pub struct Http2ChannelTypes;

impl ChannelTypes for Http2ChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = self::SendError;

    type RecvError = self::RecvError;

    type OpenBiError = self::OpenBiError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = self::AcceptBiError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type CreateChannelError = self::CreateChannelError;

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

                        let out_tx = crate::mem::SendSink(out_tx.into_sink());
                        let in_rx = crate::mem::RecvStream(in_rx.into_stream());
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

impl<In: RpcMessage, Out: RpcMessage> crate::ClientChannel<In, Out, Http2ChannelTypes>
    for ClientChannel<In, Out>
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

impl<In: RpcMessage, Out: RpcMessage> crate::ServerChannel<In, Out, Http2ChannelTypes>
    for ServerChannel<In, Out>
{
    fn accept_bi(&self) -> AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture::new(Ok(self.0.recv_async()))
    }
}
