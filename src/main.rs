use std::{result, pin::Pin, io};

use anyhow::Context;
use futures::{future::BoxFuture, Future, FutureExt, Sink, Stream, StreamExt, SinkExt, channel::mpsc, stream::BoxStream};
use serde::{Serialize, Deserialize, de::DeserializeOwned};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedWrite, FramedRead, LengthDelimitedCodec};
use pin_project::pin_project;


trait InteractionPattern {}

trait Msg<Service, Req, Res>: Into<Req> {
    type Request: Send + Into<Req>;
    type Response: Send + TryFrom<Res>;
    type Pattern: InteractionPattern;
}

struct FireAndForget;
impl InteractionPattern for FireAndForget {}
struct Rpc;
impl InteractionPattern for Rpc {}
struct ClientStreaming;
impl InteractionPattern for ClientStreaming {}
struct ServerStreaming;
impl InteractionPattern for ServerStreaming {}
struct BidiStreaming;
impl InteractionPattern for BidiStreaming {}

enum NoError {}

#[pin_project]
struct WrapNoError<S> {
    #[pin]
    inner: S,
}

impl<S: Stream + Unpin> Stream for WrapNoError<S> {
    type Item = Result<S::Item, NoError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(x)) => std::task::Poll::Ready(Some(Ok(x))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

enum Never {}
type NoSink = futures::sink::Drain<Never>;
type OneImmediateResponse<T> = futures::stream::Once<futures::future::Ready<T>>;

trait Channel<Req, Res> {
    type Sink: Sink<Req, Error = Self::SendError>;
    type Stream: Stream<Item = result::Result<Res, Self::RecvError>>;
    /// Error you might get when opening a new stream
    type ConnectionError;
    /// Error you might get while sending messages from a stream
    type SendError;
    /// Error you might get while receiving messages from a stream
    type RecvError;

    type OpenBiFuture<'a>: Future<Output = result::Result<(Self::Sink, Self::Stream), Self::ConnectionError>> + 'a where Self: 'a;
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;

    type AcceptBiFuture<'a>: Future<Output = result::Result<(Self::Sink, Self::Stream), Self::ConnectionError>> + 'a where Self: 'a;
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}

type MemSocket<Req, Res> = (
    mpsc::Sender<Req>,
    WrapNoError<mpsc::Receiver<Res>>,
);

type QuinnSocket<Req, Res> = (
    Pin<Box<dyn Sink<Req, Error = io::Error> + Send>>,
    BoxStream<'static, result::Result<Res, io::Error>>,
);

struct MemChannel<Req, Res> {
    stream: mpsc::Receiver<MemSocket<Req, Res>>,
    sink: mpsc::Sender<MemSocket<Res, Req>>,
}

impl<Req: Serialize + Send + 'static, Res: DeserializeOwned + Send + 'static> Channel<Req, Res> for quinn::Connection {
    type Sink = Pin<Box<dyn Sink<Req, Error = Self::SendError> + Send>>;

    type Stream = Pin<Box<dyn Stream<Item = result::Result<Res, Self::RecvError>> + Send>>;

    type ConnectionError = quinn::ConnectionError;

    type SendError = std::io::Error;

    type RecvError = std::io::Error;

    type OpenBiFuture<'a> = BoxFuture<'a, result::Result<QuinnSocket<Req, Res>, Self::ConnectionError>>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let this = self.clone();
        async move {
            let conn: result::Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError> = quinn::Connection::open_bi(&this).await;
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<Res>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Req>::default());
            // box so we don't have to write down the insanely long type
            let send: Pin<Box<dyn Sink<Req, Error = std::io::Error> + Send>> = Box::pin(send);
            let recv: BoxStream<'static, result::Result<Res, std::io::Error>> = Box::pin(recv);
            Ok((send, recv))
        }.boxed()
    }

    type AcceptBiFuture<'a> = BoxFuture<'a, result::Result<QuinnSocket<Req, Res>, Self::ConnectionError>>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        let this = self.clone();
        async move {
            let conn: result::Result<(quinn::SendStream, quinn::RecvStream), quinn::ConnectionError> = quinn::Connection::accept_bi(&this).await;
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<Res>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Req>::default());
            // box so we don't have to write down the insanely long type
            let send: Pin<Box<dyn Sink<Req, Error = std::io::Error> + Send>> = Box::pin(send);
            let recv: BoxStream<'static, result::Result<Res, std::io::Error>> = Box::pin(recv);
            Ok((send, recv))
        }.boxed()
    }
}

impl<Req: Send + 'static, Res: Send + 'static> Channel<Req, Res> for MemChannel<Req, Res> {

    type Sink = mpsc::Sender<Req>;

    type Stream = WrapNoError<mpsc::Receiver<Res>>;

    type ConnectionError = anyhow::Error;

    type SendError = mpsc::SendError;

    type RecvError = NoError;

    type OpenBiFuture<'a> = BoxFuture<'a, anyhow::Result<MemSocket<Req, Res>>>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let (local_send, remote_recv) = mpsc::channel::<Req>(1);
        let (remote_send, local_recv) = mpsc::channel::<Res>(1);
        async {
            let remote_recv = WrapNoError { inner: remote_recv };
            let local_recv = WrapNoError { inner: local_recv };
            // try to send the remote end
            self.sink.send((remote_send, remote_recv)).await?;
            // keep the local end and return it
            anyhow::Ok((local_send, local_recv))
        }.boxed()
    }

    type AcceptBiFuture<'a> = BoxFuture<'a, anyhow::Result<MemSocket<Req, Res>>>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        async {
            Ok(self.stream.next().await.context("no more connections")?)
        }.boxed()
    }
}

// trait Service {}
// struct MyService;

// enum Req {
//     FireAndForget(FireAndForget),
// }

// enum Res {

// }

// impl Msg<MyService, Req, Res> for FireAndForget {
//     type Request = Never;
//     type Response = ();
//     type Pattern = FireAndForget;
// }

// trait Client<S: Service, Req, Res> {
//     fn send<M>(&self, msg: M)
//     where
//         M: Msg<S, Req, Res, Pattern = FireAndForget>;

//     type RpcFuture<T>: Future<Output = T> + Send;
//     fn rpc<M>(msg: M) -> Self::RpcFuture<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = Rpc>;

//     type ClientSink<T>: Sink<T> + Send;
//     type ClientFuture<T>: Future<Output = T> + Send;
//     fn client_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::ClientSink<M::Request>,
//         Self::ClientFuture<M::Response>,
//     )
//     where
//         M: Msg<S, Req, Res, Pattern = ClientStreaming>;

//     type ServerStream<T>: Stream<Item = T> + Send;
//     fn server_streaming<M>(msg: M) -> Self::ServerStream<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = ServerStreaming>;

//     type BidiSink<T>: Sink<T> + Send;
//     type BidiStream<T>: Stream<Item = T> + Send;
//     fn bidi_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::BidiSink<M::Request>,
//         Self::BidiStream<M::Response>,
//     )
//     where
//         M: Msg<S, Req, Res, Pattern = BidiStreaming>;
// }

// enum BackChannel<Req, Res> {
//     None,
//     Rpc(Rpc),
//     ClientStreaming(ClientStreaming),
//     ServerStreaming(ServerStreaming),
//     BidiStreaming(BidiStreaming),
// }

// impl<U, Req, Res, S: Service> Client<S, Req, Res> for tokio::sync::mpsc::Sender<Req> {
//     fn send<M>(&self, msg: M)
//     where
//         M: Msg<S, Req, Res, Pattern = FireAndForget> {
//         tokio::sync::mpsc::Sender::send(self, msg.into()).ok();
//     }

//     type RpcFuture<T> = BoxFuture<'static, T>;

//     fn rpc<M>(msg: M) -> Self::RpcFuture<M::Response>
//     where
//         M: Msg<S, Req, Res, Pattern = Rpc>,
//     {
//         // send and sift through responses
//         todo!()
//     }

//     type ClientSink<T>;

//     type ClientFuture<T>;

//     fn client_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::ClientSink<M::Request>,
//         Self::ClientFuture<M::Response>,
//     )
//     where
//         M: Msg<S, Pattern = ClientStreaming> {
//         // send and send more, sift through responses for one msg
//         todo!()
//     }

//     type ServerStream<T>;

//     fn server_streaming<M>(msg: M) -> Self::ServerStream<M::Response>
//     where
//         M: Msg<S, Pattern = ServerStreaming> {
//         todo!()
//         // send and sift through responses until end
//     }

//     type BidiSink<T>;

//     type BidiStream<T>;

//     fn bidi_streaming<M>(
//         msg: M,
//     ) -> (
//         Self::BidiSink<M::Request>,
//         Self::BidiStream<M::Response>,
//     )
//     where
//         M: Msg<S, Pattern = BidiStreaming> {
//         // send and send more, sift through responses until end
//         todo!()
//     }
// }

fn main() {
    println!("Hello, world!");
}
