use std::{
    pin::Pin,
    task::{self, Poll},
};

use bincode::Options;
use futures::{
    Sink, SinkExt, Stream, StreamExt,
};
use pin_project::pin_project;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::LengthDelimitedCodec;

use crate::{
    RpcMessage,
};

type BincodeEncoding =
    bincode::config::WithOtherIntEncoding<bincode::DefaultOptions, bincode::config::FixintEncoding>;
/// Wrapper that wraps a bidirectional binary stream in a length delimited codec and bincode with fast fixint encoding
/// to get a bidirectional stream of rpc Messages
#[pin_project]
pub struct FramedBincodeRead<T, In>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        tokio_util::codec::FramedRead<T, tokio_util::codec::LengthDelimitedCodec>,
        In,
        tokio_serde::formats::SymmetricalBincode<In, BincodeEncoding>,
    >,
);

impl<T: AsyncRead, In: RpcMessage> FramedBincodeRead<T, In> {
    /// Wrap a socket in a length delimited codec and bincode with fast fixint encoding
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        // configure length delimited codec with max frame length
        let framing = LengthDelimitedCodec::builder()
            .max_frame_length(max_frame_length)
            .new_codec();
        // create the actual framing. This turns the AsyncRead/AsyncWrite into a Stream/Sink of Bytes/BytesMut
        let framed = tokio_util::codec::FramedRead::new(inner, framing);
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

impl<T: AsyncRead, In: RpcMessage> Stream for FramedBincodeRead<T, In> {
    type Item = Result<In, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next_unpin(cx)
    }
}

/// Wrapper that wraps a bidirectional binary stream in a length delimited codec and bincode with fast fixint encoding
/// to get a bidirectional stream of rpc Messages
#[pin_project]
pub struct FramedBincodeWrite<T, Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        tokio_util::codec::FramedWrite<T, tokio_util::codec::LengthDelimitedCodec>,
        Out,
        tokio_serde::formats::SymmetricalBincode<Out, BincodeEncoding>,
    >,
);

impl<T: AsyncWrite, Out: RpcMessage> FramedBincodeWrite<T, Out> {
    /// Wrap a socket in a length delimited codec and bincode with fast fixint encoding
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        // configure length delimited codec with max frame length
        let framing = LengthDelimitedCodec::builder()
            .max_frame_length(max_frame_length)
            .new_codec();
        // create the actual framing. This turns the AsyncRead/AsyncWrite into a Stream/Sink of Bytes/BytesMut
        let framed = tokio_util::codec::FramedWrite::new(inner, framing);
        // configure bincode with fixint encoding
        let bincode_options: BincodeEncoding =
            bincode::DefaultOptions::new().with_fixint_encoding();
        let bincode = tokio_serde::formats::SymmetricalBincode::from(bincode_options);
        // create the actual framing. This turns the Stream/Sink of Bytes/BytesMut into a Stream/Sink of In/Out
        let framed = tokio_serde::SymmetricallyFramed::new(framed, bincode);
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

impl<T: AsyncWrite, Out: RpcMessage> Sink<Out> for FramedBincodeWrite<T, Out> {
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

// fn assert_sink<T>(_: &impl Sink<T>) {}
// fn assert_stream<T>(_: &impl Stream<Item = T>) {}
