use std::{task::{self, Poll}, pin::Pin, result};

use bincode::Options;
use futures::{Stream, Sink, SinkExt, StreamExt, stream::{SplitStream, SplitSink}, future::BoxFuture, FutureExt};
use pin_project::pin_project;
use tokio::io::{AsyncWrite, AsyncRead};
use tokio_util::codec::LengthDelimitedCodec;

use crate::{RpcMessage, client2::{ChannelSource, ConnectionErrors, TypedConnection}};

type BincodeEncoding = bincode::config::WithOtherIntEncoding<bincode::DefaultOptions, bincode::config::FixintEncoding>;

/// Wrapper that wraps a bidirectional binary stream in a length delimited codec and bincode with fast fixint encoding
/// to get a bidirectional stream of rpc Messages
#[pin_project]
pub struct FramedBincode<T, In, Out>(
    #[pin]
    tokio_serde::Framed<
        tokio_util::codec::Framed<T, tokio_util::codec::LengthDelimitedCodec>,
        In,
        Out,
        tokio_serde::formats::Bincode<In, Out, BincodeEncoding>,
    >,
);

impl<T: AsyncRead + AsyncWrite, In: RpcMessage, Out: RpcMessage> FramedBincode<T, In, Out> {
    /// Wrap a socket in a length delimited codec and bincode with fast fixint encoding
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        // configure length delimited codec with max frame length
        let framing = LengthDelimitedCodec::builder()
            .max_frame_length(max_frame_length)
            .new_codec();
        // create the actual framing. This turns the AsyncRead/AsyncWrite into a Stream/Sink of Bytes/BytesMut
        let framed = tokio_util::codec::Framed::new(inner, framing);
        // configure bincode with fixint encoding
        let bincode_options: BincodeEncoding =
            bincode::DefaultOptions::new().with_fixint_encoding();
        let bincode = tokio_serde::formats::Bincode::from(bincode_options);
        // create the actual framing. This turns the Stream/Sink of Bytes/BytesMut into a Stream/Sink of In/Out
        let framed = tokio_serde::Framed::new(framed, bincode);
        Self(framed)
    }

    /// Get the underlying binary stream
    ///
    /// This can be useful if you want to drop the framing and use the underlying stream directly
    /// after exchanging some messages.
    pub fn into_inner(self) -> T {
        self.0.into_inner().into_inner()
    }
}

impl<T: AsyncRead, In: RpcMessage, Out: RpcMessage> Stream for FramedBincode<T, In, Out> {
    type Item = Result<In, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

impl<T: AsyncRead + AsyncWrite, In: RpcMessage, Out: RpcMessage> Sink<Out>
    for FramedBincode<T, In, Out>
{
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready_unpin(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        self.project().0.start_send_unpin(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush_unpin(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_close_unpin(cx)
    }
}

/// An adapter that turns a ChannelSource into a TypedConnection
#[derive(Debug)]
pub struct TypedChannelAdapter<T> {
    inner: T,
    max_frame_length: usize,
}

impl<T: ChannelSource> TypedChannelAdapter<T> {
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        Self { inner, max_frame_length }
    }
}

impl<T: ChannelSource> ConnectionErrors for TypedChannelAdapter<T> {
    type SendError = std::io::Error;

    type RecvError = std::io::Error;

    type OpenError = T::OpenError;
}

impl<In: RpcMessage, Out: RpcMessage, T: ChannelSource> TypedConnection<In, Out>
    for TypedChannelAdapter<T>
{
    type Channel = FramedBincode<T::Channel, In, Out>;

    type NextFut<'a> = BoxFuture<'a, result::Result<Self::Channel, Self::OpenError>>
        where Self: 'a;

    fn next(&self) -> Self::NextFut<'_> {
        async move {
            let channel = self.inner.next().await?;
            let wrapped = FramedBincode::new(channel, self.max_frame_length);
            Ok(wrapped)
        }
        .boxed()
    }
}

fn assert_sink<T>(_: &impl Sink<T>) {}
fn assert_stream<T>(_: &impl Stream<Item = T>) {}
