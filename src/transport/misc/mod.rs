//! Miscellaneous transport utilities

use futures_lite::stream;
use futures_sink::Sink;

use crate::{
    transport::{ConnectionErrors, ServerEndpoint},
    Service,
};
use std::{convert::Infallible, marker::PhantomData};

use super::ConnectionCommon;

/// A dummy server endpoint that does nothing
///
/// This can be useful as a default if you want to configure
/// an optional server endpoint.
#[derive(Default, Debug, Clone)]
pub struct DummyServerEndpoint<S>(PhantomData<S>);

impl<S: Service> ConnectionErrors for DummyServerEndpoint<S> {
    type OpenError = Infallible;
    type RecvError = Infallible;
    type SendError = Infallible;
}

impl<S: Service> ConnectionCommon<S::Req, S::Res> for DummyServerEndpoint<S> {
    type RecvStream = stream::Pending<Result<S::Req, Self::RecvError>>;
    type SendSink = Box<dyn Sink<S::Res, Error = Self::SendError> + Unpin + Send + Sync>;
}

impl<S: Service> ServerEndpoint<S::Req, S::Res> for DummyServerEndpoint<S> {
    async fn accept_bi(&self) -> Result<(Self::SendSink, Self::RecvStream), Self::OpenError> {
        futures_lite::future::pending().await
    }

    fn local_addr(&self) -> &[super::LocalAddr] {
        &[]
    }
}
