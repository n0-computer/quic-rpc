//! Miscellaneous transport utilities
use futures_lite::stream;
use futures_sink::Sink;

use crate::{
    transport::{ConnectionErrors, ServerEndpoint},
    RpcMessage,
};
use std::convert::Infallible;

use super::ConnectionCommon;

/// A dummy server endpoint that does nothing
///
/// This can be useful as a default if you want to configure
/// an optional server endpoint.
#[derive(Debug, Default)]
pub struct DummyServerEndpoint<In, Out> {
    _phantom: std::marker::PhantomData<(In, Out)>,
}

impl<In, Out> Clone for DummyServerEndpoint<In, Out> {
    fn clone(&self) -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionErrors for DummyServerEndpoint<In, Out> {
    type RecvError = Infallible;
    type SendError = Infallible;
    type OpenError = Infallible;
    type AcceptError = Infallible;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon for DummyServerEndpoint<In, Out> {
    type In = In;
    type Out = Out;
    type RecvStream = stream::Pending<Result<In, Self::RecvError>>;
    type SendSink = Box<dyn Sink<Out, Error = Self::SendError> + Unpin + Send + Sync>;
}

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint for DummyServerEndpoint<In, Out> {
    async fn accept(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::AcceptError> {
        futures_lite::future::pending().await
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        &[]
    }
}
