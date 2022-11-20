//! QUIC channel implementation based on quinn
use crate::{ChannelTypes, RpcMessage};
use futures::{future::BoxFuture, FutureExt, Sink, SinkExt, Stream, StreamExt};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::{io, pin::Pin, result};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::codec::{FramedRead, FramedWrite, LengthDelimitedCodec};

type Socket<In, Out> = (SendSink<Out>, RecvStream<In>);

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct SendSink<Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedWrite<::quinn::SendStream, LengthDelimitedCodec>,
        Out,
        SymmetricalBincode<Out>,
    >,
);

impl<Out: Serialize> Sink<Out> for SendSink<Out> {
    type Error = io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
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

/// A stream that wraps a quinn RecvStream with length delimiting and bincode
#[pin_project]
pub struct RecvStream<In>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        FramedRead<::quinn::RecvStream, LengthDelimitedCodec>,
        In,
        SymmetricalBincode<In>,
    >,
);

impl<In: DeserializeOwned> Stream for RecvStream<In> {
    type Item = result::Result<In, io::Error>;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

/// Error for open_bi. Currently just a quinn::ConnectionError
pub type OpenBiError = quinn::ConnectionError;

/// Error for accept_bi. Currently just a quinn::ConnectionError
pub type AcceptBiError = quinn::ConnectionError;

/// Types for quinn channels.
///
/// This exposes the types from quinn directly without attempting to wrap them.
#[derive(Debug, Clone, Copy)]
pub struct QuinnChannelTypes;

pub type OpenBiFuture<'a, In, Out> =
    BoxFuture<'a, result::Result<self::Socket<In, Out>, self::OpenBiError>>;

pub type AcceptBiFuture<'a, In, Out> =
    BoxFuture<'a, result::Result<self::Socket<In, Out>, self::AcceptBiError>>;

impl ChannelTypes for QuinnChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type OpenBiError = self::OpenBiError;

    type AcceptBiError = self::OpenBiError;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type Channel<In: RpcMessage, Out: RpcMessage> = quinn::Connection;
}

impl<In: RpcMessage, Out: RpcMessage> crate::Channel<In, Out, QuinnChannelTypes>
    for quinn::Connection
{
    fn open_bi(&mut self) -> OpenBiFuture<'_, In, Out> {
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
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            // box so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        }
        .boxed()
    }

    fn accept_bi(&mut self) -> AcceptBiFuture<'_, In, Out> {
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
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            // box so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        }
        .boxed()
    }
}
