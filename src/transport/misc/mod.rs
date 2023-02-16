//! Miscellaneous transport utilities

use crate::{
    transport::{ConnectionErrors, ServerEndpoint},
    RpcMessage,
};
use futures::{future, stream, Sink};
use std::convert::Infallible;

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

impl<In: RpcMessage, Out: RpcMessage> ServerEndpoint<In, Out> for DummyServerEndpoint {
    type RecvStream = stream::Pending<Result<In, Self::RecvError>>;

    type SendSink = Box<dyn Sink<Out, Error = Self::SendError> + Unpin + Send>;

    type AcceptBiFut = future::Pending<Result<(Self::SendSink, Self::RecvStream), Self::OpenError>>;

    fn accept_bi(&self) -> Self::AcceptBiFut {
        futures::future::pending()
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        &[]
    }
}
