#![cfg(feature = "http2")]
use std::net::SocketAddr;

use hyper::Uri;
use quic_rpc::{http2::Http2ChannelTypes, RpcClient, RpcServer};
use tokio::task::JoinHandle;

mod math;
use math::*;
mod util;

fn run_server(addr: &SocketAddr) -> JoinHandle<anyhow::Result<()>> {
    let (channel, hyper) =
        quic_rpc::http2::ServerChannel::<ComputeRequest, ComputeResponse>::new(addr).unwrap();
    let server = RpcServer::<ComputeService, Http2ChannelTypes>::new(channel);
    tokio::spawn(hyper);
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
    type C = Http2ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = quic_rpc::http2::ClientChannel::new(uri);
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
    type C = Http2ChannelTypes;
    let addr: SocketAddr = "127.0.0.1:3000".parse()?;
    let uri: Uri = "http://127.0.0.1:3000".parse()?;
    let server_handle = run_server(&addr);
    let client = quic_rpc::http2::ClientChannel::new(uri);
    smoke_test::<C>(client).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}
