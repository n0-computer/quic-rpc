use futures::{
    channel::mpsc, future::BoxFuture, Future, FutureExt, Sink, SinkExt, Stream, StreamExt,
};
use pin_project::pin_project;
use quinn::{RecvStream, SendStream};
use serde::{de::DeserializeOwned, Serialize};
use std::{io, pin::Pin, result, task::Poll};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};
mod mem_channel;
mod quinn_channel;

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

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        let mut this = self.project();
        match this.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(x)) => std::task::Poll::Ready(Some(Ok(x))),
            std::task::Poll::Ready(None) => std::task::Poll::Ready(None),
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

trait Channel<Req, Res> {
    /// Sink type
    type Sink: Sink<Req, Error = Self::SendError>;
    /// Stream type
    type Stream: Stream<Item = result::Result<Res, Self::RecvError>>;
    /// Error you might get while sending messages from a stream
    type SendError;
    /// Error you might get while receiving messages from a stream
    type RecvError;
    /// Error you might get when opening a new stream
    type OpenBiError;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<Output = result::Result<(Self::Sink, Self::Stream), Self::OpenBiError>>
        + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams
    type AcceptBiError;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<Output = result::Result<(Self::Sink, Self::Stream), Self::AcceptBiError>>
        + 'a
    where
        Self: 'a;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}

type MemSocket<Req, Res> = (mpsc::Sender<Req>, WrapNoError<mpsc::Receiver<Res>>);

type QuinnSocket<Req, Res> = (
    QuinnLengthDelimitedBincodeSink<Req>,
    QuinnLengthDelimitedBincodeStream<Res>,
);

struct MemChannel<Req, Res> {
    stream: mpsc::Receiver<MemSocket<Req, Res>>,
    sink: mpsc::Sender<MemSocket<Res, Req>>,
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
struct QuinnLengthDelimitedBincodeSink<T> {
    #[pin]
    inner: tokio_serde::Framed<
        FramedWrite<SendStream, LengthDelimitedCodec>,
        T,
        T,
        SymmetricalBincode<T>,
    >,
}

impl<T> QuinnLengthDelimitedBincodeSink<T> {
    fn new(
        inner: tokio_serde::Framed<
            FramedWrite<SendStream, LengthDelimitedCodec>,
            T,
            T,
            SymmetricalBincode<T>,
        >,
    ) -> Self {
        Self { inner }
    }
}

impl<T: Serialize> Sink<T> for QuinnLengthDelimitedBincodeSink<T> {
    type Error = io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().inner.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        self.project().inner.start_send_unpin(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().inner.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().inner.poll_close_unpin(cx)
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
struct QuinnLengthDelimitedBincodeStream<T> {
    #[pin]
    inner: tokio_serde::SymmetricallyFramed<
        FramedRead<RecvStream, LengthDelimitedCodec>,
        T,
        SymmetricalBincode<T>,
    >,
}

impl<T> QuinnLengthDelimitedBincodeStream<T> {
    fn new(
        inner: tokio_serde::SymmetricallyFramed<
            FramedRead<RecvStream, LengthDelimitedCodec>,
            T,
            SymmetricalBincode<T>,
        >,
    ) -> Self {
        Self { inner }
    }
}

impl<T: DeserializeOwned> Stream for QuinnLengthDelimitedBincodeStream<T> {
    type Item = result::Result<T, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().inner.poll_next_unpin(cx)
    }
}

impl<Req: Serialize + Send + 'static, Res: DeserializeOwned + Send + 'static> Channel<Req, Res>
    for quinn::Connection
{
    type Sink = QuinnLengthDelimitedBincodeSink<Req>;

    type Stream = QuinnLengthDelimitedBincodeStream<Res>;

    type OpenBiError = quinn::ConnectionError;

    type AcceptBiError = quinn::ConnectionError;

    type SendError = std::io::Error;

    type RecvError = std::io::Error;

    type OpenBiFuture<'a> = BoxFuture<'a, result::Result<QuinnSocket<Req, Res>, Self::OpenBiError>>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let this = self.clone();
        async move {
            let conn: result::Result<
                (quinn::SendStream, quinn::RecvStream),
                quinn::ConnectionError,
            > = quinn::Connection::open_bi(&this).await;
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<Res>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Req>::default());
            // box so we don't have to write down the insanely long type
            let send = QuinnLengthDelimitedBincodeSink::new(send);
            let recv = QuinnLengthDelimitedBincodeStream::new(recv);
            Ok((send, recv))
        }
        .boxed()
    }

    type AcceptBiFuture<'a> =
        BoxFuture<'a, result::Result<QuinnSocket<Req, Res>, Self::AcceptBiError>>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        let this = self.clone();
        async move {
            let conn: result::Result<
                (quinn::SendStream, quinn::RecvStream),
                quinn::ConnectionError,
            > = quinn::Connection::accept_bi(&this).await;
            let (send, recv) = conn?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let send = FramedWrite::new(send, LengthDelimitedCodec::new());
            let recv = FramedRead::new(recv, LengthDelimitedCodec::new());
            // now switch to streams of WantRequestUpdate and WantResponse
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<Res>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Req>::default());
            // box so we don't have to write down the insanely long type
            let send = QuinnLengthDelimitedBincodeSink::new(send);
            let recv = QuinnLengthDelimitedBincodeStream::new(recv);
            Ok((send, recv))
        }
        .boxed()
    }
}

struct PeerDropped;

#[pin_project]
struct MemOpenBiFuture<'a, Req, Res> {
    #[pin]
    inner: futures::sink::Send<'a, mpsc::Sender<MemSocket<Res, Req>>, MemSocket<Res, Req>>,
    res: Option<MemSocket<Req, Res>>,
}

impl<'a, Req, Res> MemOpenBiFuture<'a, Req, Res> {
    fn new(
        inner: futures::sink::Send<'a, mpsc::Sender<MemSocket<Res, Req>>, MemSocket<Res, Req>>,
        res: MemSocket<Req, Res>,
    ) -> Self {
        Self {
            inner,
            res: Some(res),
        }
    }
}

impl<'a, Req, Res> Future for MemOpenBiFuture<'a, Req, Res> {
    type Output = result::Result<MemSocket<Req, Res>, mpsc::SendError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        let mut this = self.project();
        match this.inner.poll_unpin(cx) {
            Poll::Ready(Ok(())) => this
                .res
                .take()
                .map(|x| Poll::Ready(Ok(x)))
                .unwrap_or(Poll::Pending),
            Poll::Ready(Err(cause)) => Poll::Ready(Err(cause)),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[pin_project]
struct MemAcceptBiFuture<'a, Req, Res>(
    #[pin] futures::stream::Next<'a, mpsc::Receiver<MemSocket<Req, Res>>>,
);

impl<'a, Req, Res> Future for MemAcceptBiFuture<'a, Req, Res> {
    type Output = result::Result<MemSocket<Req, Res>, PeerDropped>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        match self.project().0.poll_unpin(cx) {
            Poll::Ready(Some(socket)) => Poll::Ready(Ok(socket)),
            Poll::Ready(None) => Poll::Ready(Err(PeerDropped)),
            Poll::Pending => Poll::Pending,
        }
    }
}

impl<Req: Send + 'static, Res: Send + 'static> Channel<Req, Res> for MemChannel<Req, Res> {
    type Sink = mpsc::Sender<Req>;

    type Stream = WrapNoError<mpsc::Receiver<Res>>;

    type SendError = mpsc::SendError;

    type RecvError = NoError;

    type OpenBiError = mpsc::SendError;

    type OpenBiFuture<'a> = MemOpenBiFuture<'a, Req, Res>;

    fn open_bi(&mut self) -> Self::OpenBiFuture<'_> {
        let (local_send, remote_recv) = mpsc::channel::<Req>(1);
        let (remote_send, local_recv) = mpsc::channel::<Res>(1);
        let remote_recv = WrapNoError { inner: remote_recv };
        let local_recv = WrapNoError { inner: local_recv };
        let inner = self.sink.send((remote_send, remote_recv));
        MemOpenBiFuture::new(inner, (local_send, local_recv))
    }

    type AcceptBiError = PeerDropped;

    type AcceptBiFuture<'a> = MemAcceptBiFuture<'a, Req, Res>;

    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_> {
        MemAcceptBiFuture(self.stream.next())
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
