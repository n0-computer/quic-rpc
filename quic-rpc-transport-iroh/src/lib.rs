//! `iroh` transport implementation based on [iroh](https://crates.io/crates/iroh)

pub mod transport;

use futures_util::sink::SinkExt;
use futures_util::TryStreamExt;
use quic_rpc::transport::boxed::{
    AcceptFuture, BoxableConnector, BoxableListener, OpenFuture, RecvStream, SendSink,
};
use quic_rpc::transport::{Connector, Listener, LocalAddr};
use quic_rpc::{RpcMessage, Service};

/// An iroh listener for the given [`Service`]
pub type IrohListener<S> = crate::transport::IrohListener<<S as Service>::Req, <S as Service>::Res>;

/// An iroh connector for the given [`Service`]
pub type IrohConnector<S> =
    crate::transport::IrohConnector<<S as Service>::Res, <S as Service>::Req>;

impl<In: RpcMessage, Out: RpcMessage> BoxableConnector<In, Out>
    for crate::transport::IrohConnector<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableConnector<In, Out>> {
        Box::new(self.clone())
    }

    fn open_boxed(&self) -> OpenFuture<In, Out> {
        let f = Box::pin(async move {
            let (send, recv) = Connector::open(self).await?;
            // map the error types to anyhow
            let send = send.sink_map_err(anyhow::Error::from);
            let recv = recv.map_err(anyhow::Error::from);
            // return the boxed streams
            anyhow::Ok((SendSink::boxed(send), RecvStream::boxed(recv)))
        });
        OpenFuture::boxed(f)
    }
}

impl<In: RpcMessage, Out: RpcMessage> BoxableListener<In, Out>
    for crate::transport::IrohListener<In, Out>
{
    fn clone_box(&self) -> Box<dyn BoxableListener<In, Out>> {
        Box::new(self.clone())
    }

    fn accept_bi_boxed(&self) -> AcceptFuture<In, Out> {
        let f = async move {
            let (send, recv) = Listener::accept(self).await?;
            let send = send.sink_map_err(anyhow::Error::from);
            let recv = recv.map_err(anyhow::Error::from);
            anyhow::Ok((SendSink::boxed(send), RecvStream::boxed(recv)))
        };
        AcceptFuture::boxed(f)
    }

    fn local_addr(&self) -> &[LocalAddr] {
        Listener::local_addr(self)
    }
}
