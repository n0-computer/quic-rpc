//! Miscellaneous transport utilities

use futures_lite::{future, stream};
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
#[derive(Debug, Clone, Default)]
pub struct DummyServerEndpoint;

impl ConnectionErrors for DummyServerEndpoint {
    type OpenError = Infallible;
    type RecvError = Infallible;
    type SendError = Infallible;
}

impl<In: RpcMessage, Out: RpcMessage> ConnectionCommon<In, Out> for DummyServerEndpoint {
    type RecvStream = stream::Pending<Result<In, Self::RecvError>>;
    type SendSink = Box<dyn Sink<Out, Error = Self::SendError> + Unpin + Send + Sync>;
}

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint<In, Out> for DummyServerEndpoint {
    type AcceptBiFut = future::Pending<Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>;

    fn accept_bi(&self) -> Self::AcceptBiFut {
        futures_lite::future::pending()
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        &[]
    }
}
