#![cfg_attr(quicrpc_docsrs, feature(doc_cfg))]
use std::{fmt::Debug, future::Future, io, marker::PhantomData, ops::Deref};

use channel::none::NoReceiver;
use sealed::Sealed;
use serde::{de::DeserializeOwned, Serialize};
#[cfg(feature = "rpc")]
#[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
pub mod util;

/// Requirements for a RPC message
///
/// Even when just using the mem transport, we require messages to be Serializable and Deserializable.
/// Likewise, even when using the quinn transport, we require messages to be Send.
///
/// This does not seem like a big restriction. If you want a pure memory channel without the possibility
/// to also use the quinn transport, you might want to use a mpsc channel directly.
pub trait RpcMessage: Debug + Serialize + DeserializeOwned + Send + Sync + Unpin + 'static {}

impl<T> RpcMessage for T where
    T: Debug + Serialize + DeserializeOwned + Send + Sync + Unpin + 'static
{
}

/// Marker trait for a service
///
/// This is usually implemented by a zero-sized struct.
/// It has various bounds to make derives easier.
pub trait Service: Send + Sync + Debug + Clone + 'static {}

mod sealed {
    pub trait Sealed {}
}

/// Sealed marker trait for a sender
pub trait Sender: Debug + Sealed {}

/// Sealed marker trait for a receiver
pub trait Receiver: Debug + Sealed {}

/// Channels to be used for a message and service
pub trait Channels<S: Service> {
    /// The sender type, can be either spsc, oneshot or none
    type Tx: Sender;
    /// The receiver type, can be either spsc, oneshot or none
    ///
    /// For many services, the receiver is not needed, so it can be set to [`NoReceiver`].
    type Rx: Receiver;
}

mod wasm_browser {
    #![allow(dead_code)]
    pub(crate) type BoxedFuture<'a, T> =
        std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'a>>;
}
mod multithreaded {
    #![allow(dead_code)]
    pub(crate) type BoxedFuture<'a, T> =
        std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>;
}
#[cfg(not(all(target_family = "wasm", target_os = "unknown")))]
use multithreaded::*;
#[cfg(all(target_family = "wasm", target_os = "unknown"))]
use wasm_browser::*;

/// Channels that abstract over local or remote sending
pub mod channel {
    use std::io;

    /// Oneshot channel, similar to tokio's oneshot channel
    pub mod oneshot {
        use std::{fmt::Debug, future::Future, io, pin::Pin, task};

        use super::{RecvError, SendError};
        use crate::util::FusedOneshotReceiver;

        pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
            let (tx, rx) = tokio::sync::oneshot::channel();
            (tx.into(), rx.into())
        }

        pub type BoxedSender<T> = Box<
            dyn FnOnce(T) -> crate::BoxedFuture<'static, io::Result<()>> + Send + Sync + 'static,
        >;

        pub type BoxedReceiver<T> = crate::BoxedFuture<'static, io::Result<T>>;

        pub enum Sender<T> {
            Tokio(tokio::sync::oneshot::Sender<T>),
            Boxed(BoxedSender<T>),
        }

        impl<T> Debug for Sender<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::Tokio(_) => f.debug_tuple("Tokio").finish(),
                    Self::Boxed(_) => f.debug_tuple("Boxed").finish(),
                }
            }
        }

        impl<T> From<tokio::sync::oneshot::Sender<T>> for Sender<T> {
            fn from(tx: tokio::sync::oneshot::Sender<T>) -> Self {
                Self::Tokio(tx)
            }
        }

        impl<T> TryFrom<Sender<T>> for tokio::sync::oneshot::Sender<T> {
            type Error = Sender<T>;

            fn try_from(value: Sender<T>) -> Result<Self, Self::Error> {
                match value {
                    Sender::Tokio(tx) => Ok(tx),
                    Sender::Boxed(_) => Err(value),
                }
            }
        }

        impl<T> Sender<T> {
            pub async fn send(self, value: T) -> std::result::Result<(), SendError> {
                match self {
                    Sender::Tokio(tx) => tx.send(value).map_err(|_| SendError::ReceiverClosed),
                    Sender::Boxed(f) => f(value).await.map_err(SendError::from),
                }
            }
        }

        impl<T> Sender<T> {
            pub fn is_rpc(&self) -> bool
            where
                T: 'static,
            {
                match self {
                    Sender::Tokio(_) => false,
                    Sender::Boxed(_) => true,
                }
            }
        }

        impl<T> crate::sealed::Sealed for Sender<T> {}
        impl<T> crate::Sender for Sender<T> {}

        pub enum Receiver<T> {
            Tokio(FusedOneshotReceiver<T>),
            Boxed(BoxedReceiver<T>),
        }

        impl<T> Future for Receiver<T> {
            type Output = std::result::Result<T, RecvError>;

            fn poll(self: Pin<&mut Self>, cx: &mut task::Context) -> task::Poll<Self::Output> {
                match self.get_mut() {
                    Self::Tokio(rx) => Pin::new(rx).poll(cx).map_err(|_| RecvError::SenderClosed),
                    Self::Boxed(rx) => Pin::new(rx).poll(cx).map_err(RecvError::Io),
                }
            }
        }

        /// Convert a tokio oneshot receiver to a receiver for this crate
        impl<T> From<tokio::sync::oneshot::Receiver<T>> for Receiver<T> {
            fn from(rx: tokio::sync::oneshot::Receiver<T>) -> Self {
                Self::Tokio(FusedOneshotReceiver(rx))
            }
        }

        impl<T> TryFrom<Receiver<T>> for tokio::sync::oneshot::Receiver<T> {
            type Error = Receiver<T>;

            fn try_from(value: Receiver<T>) -> Result<Self, Self::Error> {
                match value {
                    Receiver::Tokio(tx) => Ok(tx.0),
                    Receiver::Boxed(_) => Err(value),
                }
            }
        }

        /// Convert a function that produces a future to a receiver for this crate
        impl<T, F, Fut> From<F> for Receiver<T>
        where
            F: FnOnce() -> Fut,
            Fut: Future<Output = io::Result<T>> + Send + 'static,
        {
            fn from(f: F) -> Self {
                Self::Boxed(Box::pin(f()))
            }
        }

        impl<T> Debug for Receiver<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::Tokio(_) => f.debug_tuple("Tokio").finish(),
                    Self::Boxed(_) => f.debug_tuple("Boxed").finish(),
                }
            }
        }

        impl<T> crate::sealed::Sealed for Receiver<T> {}
        impl<T> crate::Receiver for Receiver<T> {}
    }

    /// SPSC channel, similar to tokio's mpsc channel
    ///
    /// For the rpc case, the send side can not be cloned, hence spsc instead of mpsc.
    pub mod spsc {
        use std::{fmt::Debug, future::Future, io, pin::Pin};

        use super::{RecvError, SendError};
        use crate::RpcMessage;

        pub fn channel<T>(buffer: usize) -> (Sender<T>, Receiver<T>) {
            let (tx, rx) = tokio::sync::mpsc::channel(buffer);
            (tx.into(), rx.into())
        }

        pub enum Sender<T> {
            Tokio(tokio::sync::mpsc::Sender<T>),
            Boxed(Box<dyn BoxedSender<T>>),
        }

        impl<T> Sender<T> {
            pub fn is_rpc(&self) -> bool
            where
                T: 'static,
            {
                match self {
                    Sender::Tokio(_) => false,
                    Sender::Boxed(x) => x.is_rpc(),
                }
            }
        }

        impl<T> From<tokio::sync::mpsc::Sender<T>> for Sender<T> {
            fn from(tx: tokio::sync::mpsc::Sender<T>) -> Self {
                Self::Tokio(tx)
            }
        }

        impl<T> TryFrom<Sender<T>> for tokio::sync::mpsc::Sender<T> {
            type Error = Sender<T>;

            fn try_from(value: Sender<T>) -> Result<Self, Self::Error> {
                match value {
                    Sender::Tokio(tx) => Ok(tx),
                    Sender::Boxed(_) => Err(value),
                }
            }
        }

        pub trait BoxedSender<T>: Debug + Send + Sync + 'static {
            fn send(
                &mut self,
                value: T,
            ) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>>;

            fn try_send(
                &mut self,
                value: T,
            ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + '_>>;

            fn is_rpc(&self) -> bool;
        }

        pub trait BoxedReceiver<T>: Debug + Send + Sync + 'static {
            fn recv(&mut self) -> Pin<Box<dyn Future<Output = std::result::Result<Option<T>, RecvError>> + Send + '_>>;
        }

        impl<T> Debug for Sender<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::Tokio(_) => f.debug_tuple("Tokio").finish(),
                    Self::Boxed(_) => f.debug_tuple("Boxed").finish(),
                }
            }
        }

        impl<T: RpcMessage> Sender<T> {
            pub async fn send(&mut self, value: T) -> std::result::Result<(), SendError> {
                match self {
                    Sender::Tokio(tx) => {
                        tx.send(value).await.map_err(|_| SendError::ReceiverClosed)
                    }
                    Sender::Boxed(sink) => sink.send(value).await.map_err(SendError::from),
                }
            }

            pub async fn try_send(&mut self, value: T) -> std::result::Result<(), SendError> {
                match self {
                    Sender::Tokio(tx) => match tx.try_send(value) {
                        Ok(()) => Ok(()),
                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                            Err(SendError::ReceiverClosed)
                        }
                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => Ok(()),
                    },
                    Sender::Boxed(sink) => {
                        sink.try_send(value).await.map_err(SendError::from)?;
                        Ok(())
                    }
                }
            }
        }

        impl<T> crate::sealed::Sealed for Sender<T> {}
        impl<T> crate::Sender for Sender<T> {}

        pub enum Receiver<T> {
            Tokio(tokio::sync::mpsc::Receiver<T>),
            Boxed(Box<dyn BoxedReceiver<T>>),
        }

        impl<T: RpcMessage> Receiver<T> {
            /// Receive a message
            ///
            /// Returns Ok(None) if the sender has been dropped or the remote end has
            /// cleanly closed the connection.
            ///
            /// Returns an an io error if there was an error receiving the message.
            pub async fn recv(&mut self) -> std::result::Result<Option<T>, RecvError> {
                match self {
                    Self::Tokio(rx) => Ok(rx.recv().await),
                    Self::Boxed(rx) => Ok(rx.recv().await?),
                }
            }
        }

        impl<T> From<tokio::sync::mpsc::Receiver<T>> for Receiver<T> {
            fn from(rx: tokio::sync::mpsc::Receiver<T>) -> Self {
                Self::Tokio(rx)
            }
        }

        impl<T> TryFrom<Receiver<T>> for tokio::sync::mpsc::Receiver<T> {
            type Error = Receiver<T>;

            fn try_from(value: Receiver<T>) -> Result<Self, Self::Error> {
                match value {
                    Receiver::Tokio(tx) => Ok(tx),
                    Receiver::Boxed(_) => Err(value),
                }
            }
        }

        impl<T> Debug for Receiver<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                match self {
                    Self::Tokio(_) => f.debug_tuple("Tokio").finish(),
                    Self::Boxed(_) => f.debug_tuple("Boxed").finish(),
                }
            }
        }

        impl<T> crate::sealed::Sealed for Receiver<T> {}
        impl<T> crate::Receiver for Receiver<T> {}
    }

    /// No channels, used when no communication is needed
    pub mod none {
        use crate::sealed::Sealed;

        #[derive(Debug)]
        pub struct NoSender;
        impl Sealed for NoSender {}
        impl crate::Sender for NoSender {}

        #[derive(Debug)]
        pub struct NoReceiver;

        impl Sealed for NoReceiver {}
        impl crate::Receiver for NoReceiver {}
    }

    #[derive(Debug)]
    pub enum SendError {
        ReceiverClosed,
        Io(io::Error),
    }

    impl From<io::Error> for SendError {
        fn from(e: io::Error) -> Self {
            Self::Io(e)
        }
    }

    impl From<SendError> for io::Error {
        fn from(e: SendError) -> Self {
            match e {
                SendError::ReceiverClosed => io::Error::new(io::ErrorKind::BrokenPipe, e),
                SendError::Io(e) => e,
            }
        }
    }

    impl std::fmt::Display for SendError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                SendError::ReceiverClosed => write!(f, "receiver closed"),
                SendError::Io(e) => write!(f, "io error: {}", e),
            }
        }
    }

    impl std::error::Error for SendError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                SendError::Io(e) => Some(e),
                _ => None,
            }
        }
    }

    #[derive(Debug)]
    pub enum RecvError {
        SenderClosed,
        Io(io::Error),
    }

    impl From<io::Error> for RecvError {
        fn from(e: io::Error) -> Self {
            Self::Io(e)
        }
    }

    impl From<RecvError> for io::Error {
        fn from(e: RecvError) -> Self {
            match e {
                RecvError::Io(e) => e,
                RecvError::SenderClosed => io::Error::new(io::ErrorKind::BrokenPipe, e),
            }
        }
    }

    impl std::fmt::Display for RecvError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                RecvError::SenderClosed => write!(f, "sender closed"),
                RecvError::Io(e) => write!(f, "io error: {}", e),
            }
        }
    }

    impl std::error::Error for RecvError {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            match self {
                RecvError::SenderClosed => None,
                RecvError::Io(e) => Some(e),
            }
        }
    }
}

/// A wrapper for a message with channels to send and receive it.
/// This expands the protocol message to a full message that includes the
/// active and unserializable channels.
///
/// rx and tx can be set to an appropriate channel kind.
pub struct WithChannels<I: Channels<S>, S: Service> {
    /// The inner message.
    pub inner: I,
    /// The return channel to send the response to. Can be set to [`crate::channel::none::NoSender`] if not needed.
    pub tx: <I as Channels<S>>::Tx,
    /// The request channel to receive the request from. Can be set to [`NoReceiver`] if not needed.
    pub rx: <I as Channels<S>>::Rx,
    /// The current span where the full message was created.
    #[cfg(feature = "message_spans")]
    #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "message_spans")))]
    pub span: tracing::Span,
}

impl<I: Channels<S> + Debug, S: Service> Debug for WithChannels<I, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("")
            .field(&self.inner)
            .field(&self.tx)
            .field(&self.rx)
            .finish_non_exhaustive()
    }
}

impl<I: Channels<S>, S: Service> WithChannels<I, S> {
    pub fn parent_span_opt(&self) -> Option<&tracing::Span> {
        #[cfg(feature = "message_spans")]
        {
            Some(&self.span)
        }
        #[cfg(not(feature = "message_spans"))]
        {
            None
        }
    }
}

/// Tuple conversion from inner message and tx/rx channels to a WithChannels struct
///
/// For the case where you want both tx and rx channels.
impl<I: Channels<S>, S: Service, Tx, Rx> From<(I, Tx, Rx)> for WithChannels<I, S>
where
    I: Channels<S>,
    <I as Channels<S>>::Tx: From<Tx>,
    <I as Channels<S>>::Rx: From<Rx>,
{
    fn from(inner: (I, Tx, Rx)) -> Self {
        let (inner, tx, rx) = inner;
        Self {
            inner,
            tx: tx.into(),
            rx: rx.into(),
            #[cfg(feature = "message_spans")]
            #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "message_spans")))]
            span: tracing::Span::current(),
        }
    }
}

/// Tuple conversion from inner message and tx channel to a WithChannels struct
///
/// For the very common case where you just need a tx channel to send the response to.
impl<I, S, Tx> From<(I, Tx)> for WithChannels<I, S>
where
    I: Channels<S, Rx = NoReceiver>,
    S: Service,
    <I as Channels<S>>::Tx: From<Tx>,
{
    fn from(inner: (I, Tx)) -> Self {
        let (inner, tx) = inner;
        Self {
            inner,
            tx: tx.into(),
            rx: NoReceiver,
            #[cfg(feature = "message_spans")]
            #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "message_spans")))]
            span: tracing::Span::current(),
        }
    }
}

/// Deref so you can access the inner fields directly
impl<I: Channels<S>, S: Service> Deref for WithChannels<I, S> {
    type Target = I;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

#[derive(Debug)]
pub enum ServiceSender<M, R, S> {
    Local(LocalMpscChannel<M, S>, PhantomData<R>),
    #[cfg(feature = "rpc")]
    #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
    Remote(quinn::Endpoint, std::net::SocketAddr, PhantomData<(R, S)>),
}

impl<M, R, S> From<tokio::sync::mpsc::Sender<M>> for ServiceSender<M, R, S> {
    fn from(tx: tokio::sync::mpsc::Sender<M>) -> Self {
        Self::Local(tx.into(), PhantomData)
    }
}

impl<M, R, S> Clone for ServiceSender<M, R, S> {
    fn clone(&self) -> Self {
        match self {
            Self::Local(tx, _) => Self::Local(tx.clone(), PhantomData),
            #[cfg(feature = "rpc")]
            #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
            Self::Remote(endpoint, addr, _) => Self::Remote(endpoint.clone(), *addr, PhantomData),
        }
    }
}

impl<M, R, S> From<LocalMpscChannel<M, S>> for ServiceSender<M, R, S> {
    fn from(tx: LocalMpscChannel<M, S>) -> Self {
        Self::Local(tx, PhantomData)
    }
}

impl<M: Send + Sync + 'static, R, S: Service> ServiceSender<M, R, S> {
    pub fn local(&self) -> Option<&LocalMpscChannel<M, S>> {
        match self {
            Self::Local(tx, _) => Some(tx),
            #[cfg(feature = "rpc")]
            #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
            Self::Remote(_, _, _) => None,
        }
    }

    pub fn request(
        &self,
    ) -> impl Future<
        Output = io::Result<ServiceRequest<LocalMpscChannel<M, S>, rpc::RemoteRequest<R, S>>>,
    > + 'static {
        let cloned = match self {
            Self::Local(tx, _) => ServiceRequest::Local(tx.clone()),
            #[cfg(feature = "rpc")]
            #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
            Self::Remote(endpoint, addr, _) => ServiceRequest::Remote((endpoint.clone(), *addr)),
        };
        async move {
            match cloned {
                ServiceRequest::Local(tx) => Ok(ServiceRequest::Local(tx.clone())),
                #[cfg(feature = "rpc")]
                #[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
                ServiceRequest::Remote((endpoint, addr)) => {
                    let connection = endpoint
                        .connect(addr, "localhost")
                        .map_err(io::Error::other)?
                        .await?;
                    let (send, recv) = connection.open_bi().await?;
                    Ok(ServiceRequest::Remote(rpc::RemoteRequest::new(send, recv)))
                }
            }
        }
    }
}

#[derive(Debug)]
pub struct LocalMpscChannel<M, S>(tokio::sync::mpsc::Sender<M>, std::marker::PhantomData<S>);

impl<M, S> From<tokio::sync::mpsc::Sender<M>> for LocalMpscChannel<M, S> {
    fn from(tx: tokio::sync::mpsc::Sender<M>) -> Self {
        Self(tx, PhantomData)
    }
}

impl<M, S> Clone for LocalMpscChannel<M, S> {
    fn clone(&self) -> Self {
        Self(self.0.clone(), PhantomData)
    }
}

#[cfg(feature = "rpc")]
#[cfg_attr(quicrpc_docsrs, doc(cfg(feature = "rpc")))]
pub mod rpc {
    use std::{fmt::Debug, future::Future, io, marker::PhantomData, pin::Pin, sync::Arc};

    use quinn::ConnectionError;
    use serde::{de::DeserializeOwned, Serialize};
    use smallvec::SmallVec;
    use tokio::task::JoinSet;
    use tracing::{trace, trace_span, warn, Instrument};

    use crate::{
        channel::{
            none::NoSender,
            oneshot,
            spsc::{self, BoxedReceiver, BoxedSender}, RecvError,
        },
        util::{now_or_never, AsyncReadVarintExt, WriteVarintExt},
        RpcMessage,
    };

    #[derive(Debug)]
    pub struct RemoteRequest<R, S>(
        quinn::SendStream,
        quinn::RecvStream,
        std::marker::PhantomData<(R, S)>,
    );

    impl<R, S> RemoteRequest<R, S> {
        pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
            Self(send, recv, PhantomData)
        }

        pub async fn write(self, msg: impl Into<R>) -> io::Result<(RemoteWrite, RemoteRead)>
        where
            R: Serialize,
        {
            let RemoteRequest(mut send, recv, _) = self;
            let msg = msg.into();
            let mut buf = SmallVec::<[u8; 128]>::new();
            buf.write_length_prefixed(msg)?;
            send.write_all(&buf).await?;
            Ok((RemoteWrite(send), RemoteRead(recv)))
        }
    }

    #[derive(Debug)]
    pub struct RemoteRead(quinn::RecvStream);

    impl RemoteRead {
        pub(crate) fn new(recv: quinn::RecvStream) -> Self {
            Self(recv)
        }
    }

    impl<T: DeserializeOwned> From<RemoteRead> for oneshot::Receiver<T> {
        fn from(read: RemoteRead) -> Self {
            let fut = async move {
                let mut read = read.0;
                let size = read.read_varint_u64().await?.ok_or(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "failed to read size",
                ))?;
                let rest = read
                    .read_to_end(size as usize)
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                let msg: T = postcard::from_bytes(&rest)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                io::Result::Ok(msg)
            };
            oneshot::Receiver::from(|| fut)
        }
    }

    impl<T: RpcMessage> From<RemoteRead> for spsc::Receiver<T> {
        fn from(read: RemoteRead) -> Self {
            let read = read.0;
            spsc::Receiver::Boxed(Box::new(QuinnReceiver {
                recv: read,
                _marker: PhantomData,
            }))
        }
    }

    #[derive(Debug)]
    pub struct RemoteWrite(quinn::SendStream);

    impl RemoteWrite {
        pub(crate) fn new(send: quinn::SendStream) -> Self {
            Self(send)
        }
    }

    impl From<RemoteWrite> for NoSender {
        fn from(write: RemoteWrite) -> Self {
            let _ = write;
            NoSender
        }
    }

    impl<T: RpcMessage> From<RemoteWrite> for oneshot::Sender<T> {
        fn from(write: RemoteWrite) -> Self {
            let mut writer = write.0;
            oneshot::Sender::Boxed(Box::new(move |value| {
                Box::pin(async move {
                    // write via a small buffer to avoid allocation for small values
                    let mut buf = SmallVec::<[u8; 128]>::new();
                    buf.write_length_prefixed(value)?;
                    writer.write_all(&buf).await?;
                    io::Result::Ok(())
                })
            }))
        }
    }

    impl<T: RpcMessage> From<RemoteWrite> for spsc::Sender<T> {
        fn from(write: RemoteWrite) -> Self {
            let write = write.0;
            spsc::Sender::Boxed(Box::new(QuinnSender {
                send: write,
                buffer: SmallVec::new(),
                _marker: PhantomData,
            }))
        }
    }

    struct QuinnReceiver<T> {
        recv: quinn::RecvStream,
        _marker: std::marker::PhantomData<T>,
    }

    impl<T> Debug for QuinnReceiver<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("QuinnReceiver").finish()
        }
    }

    impl<T: RpcMessage> BoxedReceiver<T> for QuinnReceiver<T> {
        fn recv(&mut self) -> Pin<Box<dyn Future<Output = std::result::Result<Option<T>, RecvError>> + Send + '_>> {
            Box::pin(async {
                let read = &mut self.recv;
                let Some(size) = read.read_varint_u64().await? else {
                    return Ok(None);
                };
                let mut buf = vec![0; size as usize];
                read.read_exact(&mut buf)
                    .await
                    .map_err(|e| io::Error::new(io::ErrorKind::UnexpectedEof, e))?;
                let msg: T = postcard::from_bytes(&buf)
                    .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                Ok(Some(msg))
            })
        }
    }

    impl<T> Drop for QuinnReceiver<T> {
        fn drop(&mut self) {}
    }

    struct QuinnSender<T> {
        send: quinn::SendStream,
        buffer: SmallVec<[u8; 128]>,
        _marker: std::marker::PhantomData<T>,
    }

    impl<T> Debug for QuinnSender<T> {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("QuinnSender").finish()
        }
    }

    impl<T: RpcMessage> BoxedSender<T> for QuinnSender<T> {
        fn send(&mut self, value: T) -> Pin<Box<dyn Future<Output = io::Result<()>> + Send + '_>> {
            Box::pin(async {
                let value = value;
                self.buffer.clear();
                self.buffer.write_length_prefixed(value)?;
                self.send.write_all(&self.buffer).await?;
                self.buffer.clear();
                Ok(())
            })
        }

        fn try_send(
            &mut self,
            value: T,
        ) -> Pin<Box<dyn Future<Output = io::Result<bool>> + Send + '_>> {
            Box::pin(async {
                // todo: move the non-async part out of the box. Will require a new return type.
                let value = value;
                self.buffer.clear();
                self.buffer.write_length_prefixed(value)?;
                let Some(n) = now_or_never(self.send.write(&self.buffer)) else {
                    return Ok(false);
                };
                let n = n?;
                self.send.write_all(&self.buffer[n..]).await?;
                self.buffer.clear();
                Ok(true)
            })
        }

        fn is_rpc(&self) -> bool {
            true
        }
    }

    impl<T> Drop for QuinnSender<T> {
        fn drop(&mut self) {
            self.send.finish().ok();
        }
    }

    /// Type alias for a handler fn for remote requests
    pub type Handler<R> = Arc<
        dyn Fn(R, RemoteRead, RemoteWrite) -> crate::BoxedFuture<'static, io::Result<()>>
            + Send
            + Sync
            + 'static,
    >;

    /// Utility function to listen for incoming connections and handle them with the provided handler
    pub async fn listen<R: DeserializeOwned + 'static>(
        endpoint: quinn::Endpoint,
        handler: Handler<R>,
    ) {
        let mut request_id = 0u64;
        let mut tasks = JoinSet::new();
        while let Some(incoming) = endpoint.accept().await {
            let handler = handler.clone();
            let fut = async move {
                let connection = match incoming.await {
                    Ok(connection) => connection,
                    Err(cause) => {
                        warn!("failed to accept connection {cause:?}");
                        return io::Result::Ok(());
                    }
                };
                loop {
                    let (send, mut recv) = match connection.accept_bi().await {
                        Ok((s, r)) => (s, r),
                        Err(ConnectionError::ApplicationClosed(cause))
                            if cause.error_code.into_inner() == 0 =>
                        {
                            trace!("remote side closed connection {cause:?}");
                            return Ok(());
                        }
                        Err(cause) => {
                            warn!("failed to accept bi stream {cause:?}");
                            return Err(cause.into());
                        }
                    };
                    let size = recv.read_varint_u64().await?.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::UnexpectedEof, "failed to read size")
                    })?;
                    let mut buf = vec![0; size as usize];
                    recv.read_exact(&mut buf)
                        .await
                        .map_err(|e| io::Error::new(io::ErrorKind::UnexpectedEof, e))?;
                    let msg: R = postcard::from_bytes(&buf)
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
                    let rx = RemoteRead::new(recv);
                    let tx = RemoteWrite::new(send);
                    handler(msg, rx, tx).await?;
                }
            };
            let span = trace_span!("rpc", id = request_id);
            tasks.spawn(fut.instrument(span));
            request_id += 1;
        }
    }
}

#[derive(Debug)]
pub enum ServiceRequest<L, R> {
    Local(L),
    Remote(R),
}

impl<M: Send, S: Service> LocalMpscChannel<M, S> {
    /// Send a message to the service
    pub fn send<T>(&self, value: impl Into<WithChannels<T, S>>) -> SendFut<M>
    where
        T: Channels<S>,
        M: From<WithChannels<T, S>>,
    {
        let value: M = value.into().into();
        SendFut::new(self.0.clone(), value)
    }

    /// Send a message to the service without the type conversion magic
    pub fn send_raw(&self, value: M) -> SendFut<M> {
        SendFut::new(self.0.clone(), value)
    }
}

mod send_fut {
    use std::{
        future::Future,
        io,
        pin::Pin,
        task::{Context, Poll},
    };

    use tokio::sync::mpsc::Sender;
    use tokio_util::sync::PollSender;

    pub struct SendFut<T: Send> {
        poll_sender: PollSender<T>,
        value: Option<T>,
    }

    impl<T: Send> SendFut<T> {
        pub fn new(sender: Sender<T>, value: T) -> Self {
            Self {
                poll_sender: PollSender::new(sender),
                value: Some(value),
            }
        }
    }

    impl<T: Send + Unpin> Future for SendFut<T> {
        type Output = io::Result<()>;

        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
            let this = self.get_mut();

            // Safely extract the value
            let value = match this.value.take() {
                Some(v) => v,
                None => return Poll::Ready(Ok(())), // Already completed
            };

            // Try to reserve capacity
            match this.poll_sender.poll_reserve(cx) {
                Poll::Ready(Ok(())) => {
                    // Send the item
                    this.poll_sender.send_item(value).ok();
                    Poll::Ready(Ok(()))
                }
                Poll::Ready(Err(_)) => {
                    // Channel is closed
                    Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "Channel closed",
                    )))
                }
                Poll::Pending => {
                    // Restore the value and wait
                    this.value = Some(value);
                    Poll::Pending
                }
            }
        }
    }
}
use send_fut::SendFut;
