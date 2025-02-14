//! Transport that combines two other transports
use std::{
    error, fmt,
    fmt::Debug,
    pin::Pin,
    task::{Context, Poll},
};

use futures_lite::Stream;
use futures_sink::Sink;
use pin_project::pin_project;

use super::{ConnectionErrors, Connector, Listener, LocalAddr, StreamTypes};

/// A connection that combines two other connections
#[derive(Debug, Clone)]
pub struct CombinedConnector<A, B> {
    /// First connection
    pub a: Option<A>,
    /// Second connection
    pub b: Option<B>,
}

impl<A: Connector, B: Connector<In = A::In, Out = A::Out>> CombinedConnector<A, B> {
    /// Create a combined connection from two other connections
    ///
    /// It will always use the first connection that is not `None`.
    pub fn new(a: Option<A>, b: Option<B>) -> Self {
        Self { a, b }
    }
}

/// An endpoint that combines two other endpoints
#[derive(Debug, Clone)]
pub struct CombinedListener<A, B> {
    /// First endpoint
    pub a: Option<A>,
    /// Second endpoint
    pub b: Option<B>,
    /// Local addresses from all endpoints
    local_addr: Vec<LocalAddr>,
}

impl<A: Listener, B: Listener<In = A::In, Out = A::Out>> CombinedListener<A, B> {
    /// Create a combined listener from two other listeners
    ///
    /// When listening for incoming connections with
    /// [`Listener::accept`], all configured channels will be listened on,
    /// and the first to receive a connection will be used. If no channels are configured,
    /// accept will not throw an error but just wait forever.
    pub fn new(a: Option<A>, b: Option<B>) -> Self {
        let mut local_addr = Vec::with_capacity(2);
        if let Some(a) = &a {
            local_addr.extend(a.local_addr().iter().cloned())
        };
        if let Some(b) = &b {
            local_addr.extend(b.local_addr().iter().cloned())
        };
        Self { a, b, local_addr }
    }

    /// Get back the inner endpoints
    pub fn into_inner(self) -> (Option<A>, Option<B>) {
        (self.a, self.b)
    }
}

/// Send sink for combined channels
#[pin_project(project = SendSinkProj)]
pub enum SendSink<A: StreamTypes, B: StreamTypes> {
    /// A variant
    A(#[pin] A::SendSink),
    /// B variant
    B(#[pin] B::SendSink),
}

impl<A: StreamTypes, B: StreamTypes<In = A::In, Out = A::Out>> Sink<A::Out> for SendSink<A, B> {
    type Error = self::SendError<A, B>;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_ready(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_ready(cx).map_err(Self::Error::B),
        }
    }

    fn start_send(self: Pin<&mut Self>, item: A::Out) -> Result<(), Self::Error> {
        match self.project() {
            SendSinkProj::A(sink) => sink.start_send(item).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.start_send(item).map_err(Self::Error::B),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_flush(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_flush(cx).map_err(Self::Error::B),
        }
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        match self.project() {
            SendSinkProj::A(sink) => sink.poll_close(cx).map_err(Self::Error::A),
            SendSinkProj::B(sink) => sink.poll_close(cx).map_err(Self::Error::B),
        }
    }
}

/// RecvStream for combined channels
#[pin_project(project = ResStreamProj)]
pub enum RecvStream<A: StreamTypes, B: StreamTypes> {
    /// A variant
    A(#[pin] A::RecvStream),
    /// B variant
    B(#[pin] B::RecvStream),
}

impl<A: StreamTypes, B: StreamTypes<In = A::In, Out = A::Out>> Stream for RecvStream<A, B> {
    type Item = Result<A::In, RecvError<A, B>>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        match self.project() {
            ResStreamProj::A(stream) => stream.poll_next(cx).map_err(RecvError::<A, B>::A),
            ResStreamProj::B(stream) => stream.poll_next(cx).map_err(RecvError::<A, B>::B),
        }
    }
}

/// SendError for combined channels
#[derive(Debug)]
pub enum SendError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::SendError),
    /// B variant
    B(B::SendError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for SendError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for SendError<A, B> {}

/// RecvError for combined channels
#[derive(Debug)]
pub enum RecvError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::RecvError),
    /// B variant
    B(B::RecvError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for RecvError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for RecvError<A, B> {}

/// OpenError for combined channels
#[derive(Debug)]
pub enum OpenError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::OpenError),
    /// B variant
    B(B::OpenError),
    /// None of the two channels is configured
    NoChannel,
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for OpenError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for OpenError<A, B> {}

/// AcceptError for combined channels
#[derive(Debug)]
pub enum AcceptError<A: ConnectionErrors, B: ConnectionErrors> {
    /// A variant
    A(A::AcceptError),
    /// B variant
    B(B::AcceptError),
}

impl<A: ConnectionErrors, B: ConnectionErrors> fmt::Display for AcceptError<A, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self, f)
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> error::Error for AcceptError<A, B> {}

impl<A: ConnectionErrors, B: ConnectionErrors> ConnectionErrors for CombinedConnector<A, B> {
    type SendError = self::SendError<A, B>;
    type RecvError = self::RecvError<A, B>;
    type OpenError = self::OpenError<A, B>;
    type AcceptError = self::AcceptError<A, B>;
}

impl<A: Connector, B: Connector<In = A::In, Out = A::Out>> StreamTypes for CombinedConnector<A, B> {
    type In = A::In;
    type Out = A::Out;
    type RecvStream = self::RecvStream<A, B>;
    type SendSink = self::SendSink<A, B>;
}

impl<A: Connector, B: Connector<In = A::In, Out = A::Out>> Connector for CombinedConnector<A, B> {
    async fn open(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        let this = self.clone();
        // try a first, then b
        if let Some(a) = this.a {
            let (send, recv) = a.open().await.map_err(OpenError::A)?;
            Ok((SendSink::A(send), RecvStream::A(recv)))
        } else if let Some(b) = this.b {
            let (send, recv) = b.open().await.map_err(OpenError::B)?;
            Ok((SendSink::B(send), RecvStream::B(recv)))
        } else {
            Err(OpenError::NoChannel)
        }
    }
}

impl<A: ConnectionErrors, B: ConnectionErrors> ConnectionErrors for CombinedListener<A, B> {
    type SendError = self::SendError<A, B>;
    type RecvError = self::RecvError<A, B>;
    type OpenError = self::OpenError<A, B>;
    type AcceptError = self::AcceptError<A, B>;
}

impl<A: Listener, B: Listener<In = A::In, Out = A::Out>> StreamTypes for CombinedListener<A, B> {
    type In = A::In;
    type Out = A::Out;
    type RecvStream = self::RecvStream<A, B>;
    type SendSink = self::SendSink<A, B>;
}

impl<A: Listener, B: Listener<In = A::In, Out = A::Out>> Listener for CombinedListener<A, B> {
    async fn accept(&mut self) -> Result<(Self::SendSink, Self::RecvStream), Self::AcceptError> {
        let a_fut = async {
            if let Some(a) = &mut self.a {
                let (send, recv) = a.accept().await.map_err(AcceptError::A)?;
                Ok((SendSink::A(send), RecvStream::A(recv)))
            } else {
                std::future::pending().await
            }
        };
        let b_fut = async {
            if let Some(b) = &mut self.b {
                let (send, recv) = b.accept().await.map_err(AcceptError::B)?;
                Ok((SendSink::B(send), RecvStream::B(recv)))
            } else {
                std::future::pending().await
            }
        };
        async move {
            tokio::select! {
                res = a_fut => res,
                res = b_fut => res,
            }
        }
        .await
    }

    fn local_addr(&self) -> &[LocalAddr] {
        &self.local_addr
    }
}

#[cfg(test)]
#[cfg(feature = "flume-transport")]
mod tests {
    use crate::transport::{
        combined::{self, OpenError},
        flume, Connector,
    };

    #[tokio::test]
    async fn open_empty_channel() {
        let channel = combined::CombinedConnector::<
            flume::FlumeConnector<(), ()>,
            flume::FlumeConnector<(), ()>,
        >::new(None, None);
        let res = channel.open().await;
        assert!(matches!(res, Err(OpenError::NoChannel)));
    }
}
