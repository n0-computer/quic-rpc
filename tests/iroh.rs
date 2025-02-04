#![cfg(feature = "iroh-transport")]

use iroh::{NodeAddr, SecretKey};
use quic_rpc::{transport, RpcClient, RpcServer};
use testresult::TestResult;

use crate::transport::iroh::{IrohConnector, IrohListener};

mod math;
use math::*;
use tokio_util::task::AbortOnDropHandle;
mod util;

const ALPN: &[u8] = b"quic-rpc/iroh/test";

/// Constructs an iroh endpoint
///
/// ## Args
///
/// - alpn: the ALPN protocol to use
pub async fn make_endpoint(secret_key: SecretKey, alpn: &[u8]) -> anyhow::Result<iroh::Endpoint> {
    iroh::Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![alpn.to_vec()])
        .bind()
        .await
}

pub struct Endpoints {
    client: iroh::Endpoint,
    server: iroh::Endpoint,
    server_node_addr: NodeAddr,
}

impl Endpoints {
    pub async fn new() -> anyhow::Result<Self> {
        let server = make_endpoint(SecretKey::generate(rand::thread_rng()), ALPN).await?;

        Ok(Endpoints {
            client: make_endpoint(SecretKey::generate(rand::thread_rng()), ALPN).await?,
            server_node_addr: server.node_addr().await?,
            server,
        })
    }
}

fn run_server(server: iroh::Endpoint) -> AbortOnDropHandle<()> {
    let connection = IrohListener::new(server).unwrap();
    let server = RpcServer::new(connection);
    ComputeService::server(server)
}

// #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tokio::test]
async fn iroh_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();

    let Endpoints {
        client,
        server,
        server_node_addr,
    } = Endpoints::new().await?;
    tracing::debug!("Starting server");
    let _server_handle = run_server(server);
    tracing::debug!("Starting client");

    let client = RpcClient::new(IrohConnector::new(client, server_node_addr, ALPN.into()));
    tracing::debug!("Starting benchmark");
    bench(client, 50000).await?;
    Ok(())
}

#[tokio::test]
async fn iroh_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_node_addr,
    } = Endpoints::new().await?;
    let _server_handle = run_server(server);
    let client_connection = IrohConnector::new(client, server_node_addr, ALPN.into());
    smoke_test(client_connection).await?;
    Ok(())
}

/// Test that using the client after the server goes away and comes back behaves as if the server
/// had never gone away in the first place.
///
/// This is a regression test.
#[tokio::test]
async fn server_away_and_back() -> TestResult<()> {
    tracing_subscriber::fmt::try_init().ok();
    tracing::info!("Creating endpoints");

    let client_endpoint = make_endpoint(SecretKey::generate(rand::thread_rng()), ALPN).await?;

    let server_secret_key = SecretKey::generate(rand::thread_rng());
    let server_node_id = server_secret_key.public();

    // create the RPC client
    let client_connection = transport::iroh::IrohConnector::<ComputeResponse, ComputeRequest>::new(
        client_endpoint.clone(),
        server_node_id,
        ALPN.into(),
    );
    let client = RpcClient::<
        ComputeService,
        transport::iroh::IrohConnector<ComputeResponse, ComputeRequest>,
    >::new(client_connection);

    // send a request. No server available so it should fail
    client.rpc(Sqr(4)).await.unwrap_err();

    let server_endpoint = make_endpoint(server_secret_key.clone(), ALPN).await?;

    // create the RPC Server
    let connection = transport::iroh::IrohListener::new(server_endpoint.clone())?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::spawn(ComputeService::server_bounded(server, 1));

    // wait a bit for connection due to Windows test failing on CI
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Passing the server node address directly to client endpoint to not depend
    // on a discovery service
    let addr = server_endpoint.node_addr().await?;
    println!("adding addr {:?}", addr);
    client_endpoint.add_node_addr(addr)?;

    // send the first request and wait for the response to ensure everything works as expected
    let SqrResponse(response) = client.rpc(Sqr(4)).await?;
    assert_eq!(response, 16);

    println!("shutting down");
    let server = server_handle.await??;
    drop(server);
    server_endpoint.close().await;

    // wait for drop to free the socket
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // send a request. No server available so it should fail
    client.rpc(Sqr(4)).await.unwrap_err();

    println!("creating new endpoint");
    let server_endpoint = make_endpoint(server_secret_key.clone(), ALPN).await?;

    // make the server run again
    let connection = transport::iroh::IrohListener::new(server_endpoint.clone())?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::spawn(ComputeService::server_bounded(server, 5));

    // wait a bit for connection due to Windows test failing on CI
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Passing the server node address directly to client endpoint to not depend
    // on a discovery service
    let addr = server_endpoint.node_addr().await?;
    println!("adding addr {:?}", addr);
    client_endpoint.add_node_addr(addr)?;

    // server is running, this should work
    let SqrResponse(response) = client.rpc(Sqr(3)).await?;
    assert_eq!(response, 9);
    server_handle.abort();
    Ok(())
}
