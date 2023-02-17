#![cfg(feature = "s2n-quic-transport")]
mod math;
use std::{net::SocketAddr, path::Path};

use libp2p_core::identity::ed25519::Keypair;
use math::*;
mod util;
use quic_rpc::{RpcClient, RpcServer};
use tokio::task::JoinHandle;

fn run_server(server: s2n_quic::Server) -> JoinHandle<anyhow::Result<()>> {
    tokio::task::spawn(async move {
        let local_addr = server.local_addr()?;
        let channel = quic_rpc::transport::s2n_quic::S2nQuicServerEndpoint::new(server, local_addr);
        let server = RpcServer::<ComputeService, _>::new(channel);
        ComputeService::server(server).await?;
        anyhow::Ok(())
    })
}

async fn make_client_and_server_rustls() -> anyhow::Result<(
    s2n_quic::Client,
    s2n_quic::Server,
    s2n_quic::client::Connect,
)> {
    let client_keypair = libp2p_core::identity::Keypair::Ed25519(Keypair::generate());
    let client_config = libp2p_tls::make_client_config(&client_keypair, None)?;
    let client_tls = s2n_quic::provider::tls::rustls::Client::from(client_config);

    let client = s2n_quic::Client::builder()
        .with_tls(client_tls)?
        .with_io("0.0.0.0:0")?
        .start()
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let server_keypair = libp2p_core::identity::Keypair::Ed25519(Keypair::generate());
    let server_config = libp2p_tls::make_server_config(&server_keypair)?;
    let server_tls = s2n_quic::provider::tls::rustls::Server::from(server_config);

    let server = s2n_quic::Server::builder()
        .with_tls(server_tls)?
        .with_io("0.0.0.0:4433")?
        .start()
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let connect = s2n_quic::client::Connect::new(addr).with_server_name("localhost");
    Ok((client, server, connect))
}

async fn make_client_and_server_builtin() -> anyhow::Result<(
    s2n_quic::Client,
    s2n_quic::Server,
    s2n_quic::client::Connect,
)> {
    let client = s2n_quic::Client::builder()
        .with_tls(Path::new("./certs/cert.pem"))?
        .with_io("0.0.0.0:0")?
        .start()
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let server = s2n_quic::Server::builder()
        .with_tls((Path::new("./certs/cert.pem"), Path::new("./certs/key.pem")))?
        .with_io("0.0.0.0:4433")?
        .start()
        .map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let addr: SocketAddr = "127.0.0.1:4433".parse()?;
    let connect = s2n_quic::client::Connect::new(addr).with_server_name("localhost");
    Ok((client, server, connect))
}

#[tokio::test]
#[ignore]
async fn s2n_quic_channel_smoke() -> anyhow::Result<()> {
    let (client, server, connect) = make_client_and_server_builtin().await?;
    let server_handle = run_server(server);
    let client = quic_rpc::transport::s2n_quic::S2nQuicConnection::new(client, connect);
    smoke_test(client).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn s2n_quic_channel_bench() -> anyhow::Result<()> {
    let (client, server, connect) = make_client_and_server_builtin().await?;
    let server_handle = run_server(server);
    let client = quic_rpc::transport::s2n_quic::S2nQuicConnection::new(client, connect);
    let client = RpcClient::<ComputeService, _>::new(client);
    bench(client, 5).await?;
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}
