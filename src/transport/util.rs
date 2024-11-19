use std::{
    pin::Pin,
    task::{self, Poll},
};

use futures_lite::Stream;
use futures_sink::Sink;
use pin_project::pin_project;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::codec::LengthDelimitedCodec;

#[pin_project]
pub struct FramedPostcardRead<T, In>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        tokio_util::codec::FramedRead<T, tokio_util::codec::LengthDelimitedCodec>,
        In,
        tokio_serde_postcard::SymmetricalPostcard<In>,
    >,
);

impl<T: AsyncRead, In: DeserializeOwned> FramedPostcardRead<T, In> {
    /// Wrap a socket in a length delimited codec and postcard encoding
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        // configure length delimited codec with max frame length
        let framing = LengthDelimitedCodec::builder()
            .max_frame_length(max_frame_length)
            .new_codec();
        // create the actual framing. This turns the AsyncRead/AsyncWrite into a Stream/Sink of Bytes/BytesMut
        let framed = tokio_util::codec::FramedRead::new(inner, framing);
        let postcard = tokio_serde_postcard::Postcard::new();
        // create the actual framing. This turns the Stream/Sink of Bytes/BytesMut into a Stream/Sink of In/Out
        let framed = tokio_serde::Framed::new(framed, postcard);
        Self(framed)
    }
}

impl<T, In> FramedPostcardRead<T, In> {
    /// Get the underlying binary stream
    ///
    /// This can be useful if you want to drop the framing and use the underlying stream directly
    /// after exchanging some messages.
    pub fn into_inner(self) -> T {
        self.0.into_inner().into_inner()
    }
}

impl<T: AsyncRead, In: DeserializeOwned> Stream for FramedPostcardRead<T, In> {
    type Item = Result<In, std::io::Error>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut task::Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.project().0).poll_next(cx)
    }
}

/// Wrapper that wraps a bidirectional binary stream in a length delimited codec and postcard encoding
/// to get a bidirectional stream of rpc Messages
#[pin_project]
pub struct FramedPostcardWrite<T, Out>(
    #[pin]
    tokio_serde::SymmetricallyFramed<
        tokio_util::codec::FramedWrite<T, tokio_util::codec::LengthDelimitedCodec>,
        Out,
        tokio_serde_postcard::SymmetricalPostcard<Out>,
    >,
);

impl<T: AsyncWrite, Out: Serialize> FramedPostcardWrite<T, Out> {
    /// Wrap a socket in a length delimited codec and postcard encoding
    pub fn new(inner: T, max_frame_length: usize) -> Self {
        // configure length delimited codec with max frame length
        let framing = LengthDelimitedCodec::builder()
            .max_frame_length(max_frame_length)
            .new_codec();
        // create the actual framing. This turns the AsyncRead/AsyncWrite into a Stream/Sink of Bytes/BytesMut
        let framed = tokio_util::codec::FramedWrite::new(inner, framing);
        let postcard = tokio_serde_postcard::SymmetricalPostcard::new();
        // create the actual framing. This turns the Stream/Sink of Bytes/BytesMut into a Stream/Sink of In/Out
        let framed = tokio_serde::SymmetricallyFramed::new(framed, postcard);
        Self(framed)
    }
}

impl<T, Out> FramedPostcardWrite<T, Out> {
    /// Get the underlying binary stream
    ///
    /// This can be useful if you want to drop the framing and use the underlying stream directly
    /// after exchanging some messages.
    pub fn into_inner(self) -> T {
        self.0.into_inner().into_inner()
    }
}

impl<T: AsyncWrite, Out: Serialize> Sink<Out> for FramedPostcardWrite<T, Out> {
    type Error = std::io::Error;

    fn poll_ready(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
        Pin::new(&mut self.project().0).start_send(item)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_flush(cx)
    }

    fn poll_close(
        self: Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.project().0).poll_close(cx)
    }
}

mod tokio_serde_postcard {
    use {
        bytes::{BufMut as _, Bytes, BytesMut},
        pin_project::pin_project,
        serde::{Deserialize, Serialize},
        std::{io, marker::PhantomData, pin::Pin},
        tokio_serde::{Deserializer, Serializer},
    };

    #[pin_project]
    pub struct Postcard<Item, SinkItem> {
        #[pin]
        buffer: Box<Option<BytesMut>>,
        _marker: PhantomData<(Item, SinkItem)>,
    }

    impl<Item, SinkItem> Default for Postcard<Item, SinkItem> {
        fn default() -> Self {
            Self::new()
        }
    }

    impl<Item, SinkItem> Postcard<Item, SinkItem> {
        pub fn new() -> Self {
            Self {
                buffer: Box::new(None),
                _marker: PhantomData,
            }
        }
    }

    pub type SymmetricalPostcard<T> = Postcard<T, T>;

    impl<Item, SinkItem> Deserializer<Item> for Postcard<Item, SinkItem>
    where
        for<'a> Item: Deserialize<'a>,
    {
        type Error = io::Error;

        fn deserialize(self: Pin<&mut Self>, src: &BytesMut) -> Result<Item, Self::Error> {
            postcard::from_bytes(&src)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
        }
    }

    impl<Item, SinkItem> Serializer<SinkItem> for Postcard<Item, SinkItem>
    where
        SinkItem: Serialize,
    {
        type Error = io::Error;

        fn serialize(self: Pin<&mut Self>, data: &SinkItem) -> Result<Bytes, Self::Error> {
            let mut this = self.project();
            let buffer = this.buffer.take().unwrap_or_default();
            let mut buffer = postcard::to_io(data, buffer.writer())
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?
                .into_inner();
            if buffer.len() <= 1024 {
                let res = buffer.split().freeze();
                this.buffer.replace(buffer);
                Ok(res)
            } else {
                Ok(buffer.freeze())
            }
        }
    }
}

mod direct {
    use futures_lite::{ready, Stream};
    use futures_sink::Sink;
    use pin_project::pin_project;
    use postcard;
    use serde::de::DeserializeOwned;
    use serde::Serialize;
    use smallvec::SmallVec;
    use std::marker::PhantomData;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf}; // Ensure the postcard crate is included in your Cargo.toml

    #[pin_project]
    /// A Sink that encodes messages into a framed stream with 32-bit big-endian length prefixes,
    /// using Postcard for serialization.
    pub struct FramedPostcardWrite<W, Out> {
        #[pin]
        inner: W,
        buffer: SmallVec<[u8; 1024]>,
        written: usize,
        _phantom: PhantomData<Out>,
    }

    impl<W, Out> FramedPostcardWrite<W, Out>
    where
        W: AsyncWrite,
        Out: Serialize,
    {
        /// Creates a new `FramedPostcardWrite` with the provided `AsyncWrite`.
        pub fn new(inner: W) -> Self {
            Self {
                inner,
                buffer: SmallVec::new(),
                written: 0,
                _phantom: PhantomData,
            }
        }
    }

    impl<W, Out> Sink<Out> for FramedPostcardWrite<W, Out>
    where
        W: AsyncWrite,
        Out: Serialize,
    {
        type Error = std::io::Error;

        fn poll_ready(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            // Always ready to accept data.
            Poll::Ready(Ok(()))
        }

        fn start_send(self: Pin<&mut Self>, item: Out) -> Result<(), Self::Error> {
            let this = self.project();

            // Serialize the item using Postcard.
            let data = postcard::to_stdvec(&item).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("Serialization error: {}", e),
                )
            })?;
            let len = data.len();

            if len > u32::MAX as usize {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "Item too large",
                ));
            }

            // Encode length prefix in big-endian format.
            let len_u32 = len as u32;
            this.buffer.extend_from_slice(&len_u32.to_be_bytes());
            this.buffer.extend_from_slice(&data);

            Ok(())
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
            let mut this = self.project();

            while *this.written < this.buffer.len() {
                match this
                    .inner
                    .as_mut()
                    .poll_write(cx, &this.buffer[*this.written..])
                {
                    Poll::Ready(Ok(0)) => {
                        return Poll::Ready(Err(std::io::Error::new(
                            std::io::ErrorKind::WriteZero,
                            "Failed to write data",
                        )));
                    }
                    Poll::Ready(Ok(n)) => *this.written += n,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                }
            }

            // Clear the buffer once all data is written.
            this.buffer.clear();
            *this.written = 0;

            // Ensure the inner writer is flushed.
            this.inner.as_mut().poll_flush(cx)
        }

        fn poll_close(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Result<(), Self::Error>> {
            // Flush any remaining data and close the inner writer.
            ready!(self.as_mut().poll_flush(cx))?;
            self.project().inner.poll_shutdown(cx)
        }
    }

    #[pin_project]
    /// A Stream that reads framed messages from an AsyncRead, deserializing them using Postcard.
    ///
    /// Each message is expected to be prefixed with a 32-bit big-endian length field.
    pub struct FramedPostcardRead<R, In> {
        #[pin]
        inner: R,
        buffer: SmallVec<[u8; 1024]>,
        state: ReadState,
        _phantom: PhantomData<In>,
    }

    enum ReadState {
        ReadingLength { buf: [u8; 4], read: usize },
        ReadingData { len: usize, read: usize },
    }

    impl<R, In> FramedPostcardRead<R, In>
    where
        R: AsyncRead,
    {
        /// Creates a new `FramedPostcardRead` with the provided `AsyncRead`.
        pub fn new(inner: R) -> Self {
            Self {
                inner,
                buffer: SmallVec::new(),
                state: ReadState::ReadingLength {
                    buf: [0; 4],
                    read: 0,
                },
                _phantom: PhantomData,
            }
        }
    }

    impl<R, In> Stream for FramedPostcardRead<R, In>
    where
        R: AsyncRead,
        In: DeserializeOwned,
    {
        type Item = Result<In, std::io::Error>;

        fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
            let mut this = self.project();

            loop {
                match &mut this.state {
                    ReadState::ReadingLength { buf, read } => {
                        while *read < 4 {
                            let mut read_buf = ReadBuf::new(&mut buf[*read..]);
                            match this.inner.as_mut().poll_read(cx, &mut read_buf) {
                                Poll::Ready(Ok(())) => {
                                    let n = read_buf.filled().len();
                                    if n == 0 {
                                        if *read == 0 {
                                            // EOF reached without reading any data.
                                            return Poll::Ready(None);
                                        } else {
                                            return Poll::Ready(Some(Err(std::io::Error::new(
                                                std::io::ErrorKind::UnexpectedEof,
                                                "EOF while reading length prefix",
                                            ))));
                                        }
                                    }
                                    *read += n;
                                }
                                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
                                Poll::Pending => return Poll::Pending,
                            }
                        }
                        let len = u32::from_be_bytes(*buf) as usize;
                        if len > (10 * 1024 * 1024) {
                            // Arbitrary limit to prevent reading excessively large frames.
                            return Poll::Ready(Some(Err(std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "Frame size too large",
                            ))));
                        }
                        this.buffer.resize(len, 0);
                        *this.state = ReadState::ReadingData { len, read: 0 };
                    }
                    ReadState::ReadingData { len, read } => {
                        while *read < *len {
                            let mut read_buf = ReadBuf::new(&mut this.buffer[*read..]);
                            match this.inner.as_mut().poll_read(cx, &mut read_buf) {
                                Poll::Ready(Ok(())) => {
                                    let n = read_buf.filled().len();
                                    if n == 0 {
                                        return Poll::Ready(Some(Err(std::io::Error::new(
                                            std::io::ErrorKind::UnexpectedEof,
                                            "EOF while reading frame data",
                                        ))));
                                    }
                                    *read += n;
                                }
                                Poll::Ready(Err(e)) => return Poll::Ready(Some(Err(e))),
                                Poll::Pending => return Poll::Pending,
                            }
                        }
                        // Deserialize the message using Postcard.
                        let msg = postcard::from_bytes::<In>(&this.buffer)
                            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
                        // Reset state for next message.
                        *this.state = ReadState::ReadingLength {
                            buf: [0; 4],
                            read: 0,
                        };
                        this.buffer.clear();
                        return Poll::Ready(Some(Ok(msg)));
                    }
                }
            }
        }
    }
}
