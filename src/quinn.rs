use ::quinn::{RecvStream, SendStream};
use futures::{future::BoxFuture, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::{io, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Socket<Req, Res> = (ReqSink<Req>, ResStream<Res>);

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct ReqSink<Req>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedWrite<SendStream, LengthDelimitedCodec>,
        Req,
        SymmetricalBincode<Req>,
    >,
);

impl<Req: Serialize> Sink<Req> for ReqSink<Req> {
    type Error = io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Req) -> Result<(), Self::Error> {
        self.project().0.start_send_unpin(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct ResStream<T>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedRead<RecvStream, LengthDelimitedCodec>,
        T,
        SymmetricalBincode<T>,
    >,
);

impl<T: DeserializeOwned> Stream for ResStream<T> {
    type Item = result::Result<T, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

pub type OpenBiError = quinn::ConnectionError;

pub type AcceptBiError = quinn::ConnectionError;

impl<Req: Serialize + Send + 'static, Res: DeserializeOwned + Send + 'static>
    crate::Channel<Req, Res> for quinn::Connection
{
    type ReqSink = self::ReqSink<Req>;

    type ResStream = self::ResStream<Res>;

    type OpenBiError = self::OpenBiError;

    type AcceptBiError = self::OpenBiError;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiFuture<'a> =
        BoxFuture<'a, result::Result<self::Socket<Req, Res>, Self::OpenBiError>>;

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
            let send = ReqSink(send);
            let recv = ResStream(recv);
            Ok((send, recv))
        }
        .boxed()
    }

    type AcceptBiFuture<'a> =
        BoxFuture<'a, result::Result<self::Socket<Req, Res>, Self::AcceptBiError>>;

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
            let send = ReqSink(send);
            let recv = ResStream(recv);
            Ok((send, recv))
        }
        .boxed()
    }
}
