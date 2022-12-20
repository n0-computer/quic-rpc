#![cfg(feature = "http2")]
use std::{net::SocketAddr, result, error};

use futures::{Stream};
use hyper::Uri;
use quic_rpc::{transport::http2, RpcClient, RpcServer};
use tokio::{task::JoinHandle, io::AsyncRead, io::AsyncWrite};

mod math;
use math::*;
mod util;

fn run_server_local(stream: impl Stream<Item = result::Result<impl AsyncRead + AsyncWrite + Unpin + Send + 'static, impl Into<Box<dyn error::Error + Send + Sync>> + 'static>> + Send + 'static) -> JoinHandle<anyhow::Result<()>> {
    let accept = hyper::server::accept::from_stream(stream);
    let channel =
        http2::ServerChannel::<ComputeRequest, ComputeResponse>::serve_with_incoming(accept).unwrap();
    let server = RpcServer::<ComputeService, http2::ChannelTypes>::new(channel);
    tokio::spawn(async move {
        loop {
            let server = server.clone();
            ComputeService::server(server).await?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    })
}

fn run_server(addr: &SocketAddr) -> JoinHandle<anyhow::Result<()>> {
    let channel = http2::ServerChannel::<ComputeRequest, ComputeResponse>::serve(addr).unwrap();
    let server = RpcServer::<ComputeService, http2::ChannelTypes>::new(channel);
    tokio::spawn(async move {
        loop {
            let server = server.clone();
            ComputeService::server(server).await?;
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    })
}

#[tokio::test]
async fn http2_channel_bench() -> anyhow::Result<()> {
    type C = http2::ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = http2::ClientChannel::new(uri);
    let client = RpcClient::<ComputeService, C>::new(client);
    bench(client, 50000).await?;
    println!("terminating server");
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test]
#[ignore]
async fn http2_channel_smoke() -> anyhow::Result<()> {
    type C = http2::ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = http2::ClientChannel::new(uri);
    smoke_test::<C>(client).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}
