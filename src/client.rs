//! Client side api
//!
//! The main entry point is [RpcClient].
use crate::{
    map::{ChainedMapper, MapService, Mapper},
    Service, ServiceConnection,
};
use futures_lite::Stream;
use futures_sink::Sink;

use pin_project::pin_project;
use std::{
    fmt::Debug,
    marker::PhantomData,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};

/// Type alias for a boxed connection to a specific service
pub type BoxedServiceConnection<S> =
    crate::transport::boxed::Connection<<S as crate::Service>::Res, <S as crate::Service>::Req>;

/// Sync version of `future::stream::BoxStream`.
pub type BoxStreamSync<'a, T> = Pin<Box<dyn Stream<Item = T> + Send + Sync + 'a>>;

/// A client for a specific service
///
/// This is a wrapper around a [ServiceConnection] that serves as the entry point
/// for the client DSL. `S` is the service type, `C` is the substream source.
#[derive(Debug)]
pub struct RpcClient<S, SInner = S, C = BoxedServiceConnection<S>> {
    pub(crate) source: C,
    pub(crate) map: Arc<dyn MapService<S, SInner>>,
}

impl<S, SInner, C: Clone> Clone for RpcClient<S, SInner, C> {
    fn clone(&self) -> Self {
        Self {
            source: self.source.clone(),
            map: Arc::clone(&self.map),
        }
    }
}

/// Sink that can be used to send updates to the server for the two interaction patterns
/// that support it, [crate::message::ClientStreaming] and [crate::message::BidiStreaming].
#[pin_project]
#[derive(Debug)]
pub struct UpdateSink<S, C, T, SInner = S>(
    #[pin] pub C::SendSink,
    pub PhantomData<T>,
    pub Arc<dyn MapService<S, SInner>>,
)
where
    S: Service,
    SInner: Service,
    C: ServiceConnection<S>,
    T: Into<SInner::Req>;

impl<S, C, T, SInner> Sink<T> for UpdateSink<S, C, T, SInner>
where
    S: Service,
    SInner: Service,
    C: ServiceConnection<S>,
    T: Into<SInner::Req>,
{
    type Error = C::SendError;

    fn poll_ready(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_ready(cx)
    }

    fn start_send(self: Pin<&mut Self>, item: T) -> Result<(), Self::Error> {
        let req = self.2.req_into_outer(item.into());
        self.project().0.start_send(req)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_flush(cx)
    }

    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.project().0.poll_close(cx)
    }
}

impl<S, C> RpcClient<S, S, C>
where
    S: Service,
    C: ServiceConnection<S>,
{
    /// Create a new rpc client for a specific [Service] given a compatible
    /// [ServiceConnection].
    ///
    /// This is where a generic typed connection is converted into a client for a specific service.
    pub fn new(source: C) -> Self {
        Self {
            source,
            map: Arc::new(Mapper::new()),
        }
    }
}

impl<S, SInner, C> RpcClient<S, SInner, C>
where
    S: Service,
    C: ServiceConnection<S>,
    SInner: Service,
{
    /// Get the underlying connection
    pub fn into_inner(self) -> C {
        self.source
    }

    /// Map this channel's service into an inner service.
    ///
    /// This method is available if the required bounds are upheld:
    /// SNext::Req: Into<SInner::Req> + TryFrom<SInner::Req>,
    /// SNext::Res: Into<SInner::Res> + TryFrom<SInner::Res>,
    ///
    /// Where SNext is the new service to map to and SInner is the current inner service.
    ///
    /// This method can be chained infintely.
    pub fn map<SNext>(self) -> RpcClient<S, SNext, C>
    where
        SNext: Service,
        SNext::Req: Into<SInner::Req> + TryFrom<SInner::Req>,
        SNext::Res: Into<SInner::Res> + TryFrom<SInner::Res>,
    {
        let map = ChainedMapper::new(self.map);
        RpcClient {
            source: self.source,
            map: Arc::new(map),
        }
    }
}

impl<S, SInner, C> AsRef<C> for RpcClient<S, SInner, C>
where
    S: Service,
    C: ServiceConnection<S>,
    SInner: Service,
{
    fn as_ref(&self) -> &C {
        &self.source
    }
}

/// Wrap a stream with an additional item that is kept alive until the stream is dropped
#[pin_project]
pub(crate) struct DeferDrop<S: Stream, X>(#[pin] pub S, pub X);

impl<S: Stream, X> Stream for DeferDrop<S, X> {
    type Item = S::Item;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.project().0.poll_next(cx)
    }
}
