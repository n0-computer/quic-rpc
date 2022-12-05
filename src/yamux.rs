//! TCP channel implementation based on yamux
#![allow(clippy::type_complexity)]
use crate::RpcMessage;
use bytes::Bytes;
use futures::{
    stream::{SplitSink, SplitStream},
    AsyncRead, AsyncWrite, Future, Sink, SinkExt, Stream, StreamExt,
};
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use std::{
    fmt, io,
    marker::PhantomData,
    pin::Pin,
    result,
    sync::{Arc, Mutex},
    task::{Context, Poll},
};
use tokio_serde::{formats::SymmetricalBincode, SymmetricallyFramed};
use tokio_util::{
    codec::{Framed, LengthDelimitedCodec},
    compat::{Compat, FuturesAsyncReadCompatExt},
};
use yamux::Mode;

pub trait InnerConnection: AsyncRead + AsyncWrite + Unpin + Send + 'static {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> InnerConnection for T {}

trait Ops: Send + Sync + 'static {
    fn poll_new_outbound(&self, cx: &mut Context<'_>) -> Poll<yamux::Result<yamux::Stream>>;
    fn poll_next_inbound(&self, cx: &mut Context<'_>)
        -> Poll<Option<yamux::Result<yamux::Stream>>>;
}

impl<T: InnerConnection> Ops for Mutex<yamux::Connection<T>> {
    fn poll_new_outbound(&self, cx: &mut Context<'_>) -> Poll<yamux::Result<yamux::Stream>> {
        self.lock().unwrap().poll_new_outbound(cx)
    }

    fn poll_next_inbound(
        &self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<yamux::Result<yamux::Stream>>> {
        self.lock().unwrap().poll_next_inbound(cx)
    }
}

pub struct Channel<In: RpcMessage, Out: RpcMessage>(Arc<dyn Ops>, PhantomData<(In, Out)>);

impl<In: RpcMessage, Out: RpcMessage> Channel<In, Out> {
    /// Create a new channel
    pub fn new(conn: impl InnerConnection, config: yamux::Config, mode: Mode) -> Self {
        let conn = yamux::Connection::new(conn, config, mode);
        Self(Arc::new(Mutex::new(conn)), PhantomData)
    }

    pub async fn consume_incoming_streams(self) -> Result<(), yamux::ConnectionError> {
        println!("consume_incoming_streams");
        let ops = self.0;
        let mut stream = futures::stream::poll_fn(move |cx| ops.poll_next_inbound(cx));
        while let Some(stream) = stream.next().await {
            println!("got stream {:?}", stream);
            let stream = stream?;
            panic!()
        }
        Ok(())
    }
}

impl<In: RpcMessage, Out: RpcMessage> fmt::Debug for Channel<In, Out> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Channel").field(&self.1).finish()
    }
}

impl<In: RpcMessage, Out: RpcMessage> Clone for Channel<In, Out> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

/// A sink that wraps a quinn SendStream with length delimiting and bincode
#[pin_project]
pub struct SendSink<Out>(
    #[pin]
    SymmetricallyFramed<
        SplitSink<Framed<Compat<::yamux::Stream>, LengthDelimitedCodec>, Bytes>,
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
        SplitStream<Framed<Compat<::yamux::Stream>, LengthDelimitedCodec>>,
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

/// Future returned by open_bi
#[pin_project]
pub struct OpenBiFuture<'a, In, Out>(#[pin] &'a Arc<dyn Ops>, PhantomData<(In, Out)>);

impl<'a, In: RpcMessage, Out: RpcMessage> Future for OpenBiFuture<'a, In, Out> {
    type Output = std::result::Result<(SendSink<Out>, RecvStream<In>), yamux::ConnectionError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0.poll_new_outbound(cx).map(|stream| {
            println!("got open_bi stream {:?}", stream);
            let stream = stream?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let stream = Framed::new(stream.compat(), LengthDelimitedCodec::new());
            let (send, recv) = stream.split();
            // now switch to streams of WantRequestUpdate and WantResponse
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            // wrap so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        })
    }
}

#[pin_project]
pub struct AcceptBiFuture<'a, In, Out>(#[pin] &'a Arc<dyn Ops>, PhantomData<(In, Out)>);

impl<'a, In, Out> Future for AcceptBiFuture<'a, In, Out> {
    type Output = std::result::Result<(SendSink<Out>, RecvStream<In>), yamux::ConnectionError>;

    fn poll(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        self.0.poll_next_inbound(cx).map(|stream| {
            let stream = stream.unwrap()?;
            // turn chunks of bytes into a stream of messages using length delimited codec
            let stream = Framed::new(stream.compat(), LengthDelimitedCodec::new());
            let (send, recv) = stream.split();
            // now switch to streams of WantRequestUpdate and WantResponse
            let send = SymmetricallyFramed::new(send, SymmetricalBincode::<Out>::default());
            let recv = SymmetricallyFramed::new(recv, SymmetricalBincode::<In>::default());
            // wrap so we don't have to write down the insanely long type
            let send = SendSink(send);
            let recv = RecvStream(recv);
            Ok((send, recv))
        })
    }
}

impl<In: RpcMessage, Out: RpcMessage> crate::Channel<In, Out, YamuxChannelTypes>
    for Channel<In, Out>
{
    fn open_bi(&self) -> self::OpenBiFuture<'_, In, Out> {
        println!("yamux open_bi");
        OpenBiFuture(&self.0, PhantomData)
    }

    fn accept_bi(&self) -> self::AcceptBiFuture<'_, In, Out> {
        AcceptBiFuture(&self.0, PhantomData)
    }
}

#[derive(Debug, Clone)]
pub struct YamuxChannelTypes;

impl crate::ChannelTypes for YamuxChannelTypes {
    type SendSink<M: RpcMessage> = self::SendSink<M>;

    type RecvStream<M: RpcMessage> = self::RecvStream<M>;

    type SendError = io::Error;

    type RecvError = io::Error;

    type OpenBiError = yamux::ConnectionError;

    type OpenBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::OpenBiFuture<'a, In, Out>;

    type AcceptBiError = yamux::ConnectionError;

    type AcceptBiFuture<'a, In: RpcMessage, Out: RpcMessage> = self::AcceptBiFuture<'a, In, Out>;

    type CreateChannelError = io::Error;

    type Channel<In: RpcMessage, Out: RpcMessage> = self::Channel<In, Out>;
}
