//! Miscellaneous transport utilities
use std::convert::Infallible;

use futures_lite::stream;
use futures_sink::Sink;

use super::StreamTypes;
use crate::{
    transport::{ConnectionErrors, Listener},
    RpcMessage,
};

/// A dummy listener that does nothing
///
/// This can be useful as a default if you want to configure
/// an optional listener.
#[derive(Debug, Default)]
pub struct DummyListener<In, Out> {
    _p: std::marker::PhantomData<(In, Out)>,
}

impl<In, Out> Clone for DummyListener<In, Out> {
    fn clone(&self) -> Self {
        Self {
            _p: std::marker::PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for DummyListener<In, Out> {
    type RecvError = Infallible;
    type SendError = Infallible;
    type OpenError = Infallible;
    type AcceptError = Infallible;
}

impl<In: RpcMessage, Out: RpcMessage> StreamTypes for DummyListener<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = stream::Pending<Result<In, Self::RecvError>>;
    type SendSink = Box<dyn Sink<Out, Error = Self::SendError> + Unpin + Send + Sync>;
}

impl<In: RpcMessage, Out: RpcMessage> Listener for DummyListener<In, Out> {
    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::AcceptError> {
        futures_lite::future::pending().await
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        &[]
    }
}
