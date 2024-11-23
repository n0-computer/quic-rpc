#![cfg(feature = "iroh-net-transport")]

use iroh_net::{key::SecretKey, NodeAddr};
use quic_rpc::{transport, RpcClient, RpcServer};
use testresult::TestResult;

use crate::transport::iroh_net::{IrohNetConnector, IrohNetListener};

mod math;
use math::*;
use tokio_util::task::AbortOnDropHandle;
mod util;

const ALPN: &[u8] = b"quic-rpc/iroh-net/test";

/// Constructs an iroh-net endpoint
///
/// ## Args
///
/// - alpn: the ALPN protocol to use
pub async fn make_endpoint(
    secret_key: SecretKey,
    alpn: &[u8],
) -> anyhow::Result<iroh_net::Endpoint> {
    iroh_net::Endpoint::builder()
        .secret_key(secret_key)
        .alpns(vec![alpn.to_vec()])
        .bind()
        .await
}

pub struct Endpoints {
    client: iroh_net::Endpoint,
    server: iroh_net::Endpoint,
    server_node_addr: NodeAddr,
}

impl Endpoints {
    pub async fn new() -> anyhow::Result<Self> {
        let server = make_endpoint(SecretKey::generate(), ALPN).await?;

        Ok(Endpoints {
            client: make_endpoint(SecretKey::generate(), ALPN).await?,
            server_node_addr: server.node_addr().await?,
            server,
        })
    }
}

fn run_server(server: iroh_net::Endpoint) -> AbortOnDropHandle<()> {
    let connection = IrohNetListener::new(server).unwrap();
    let server = RpcServer::new(connection);
    ComputeService::server(server)
}

// #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tokio::test]
async fn iroh_net_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();

    let Endpoints {
        client,
        server,
        server_node_addr,
    } = Endpoints::new().await?;
    tracing::debug!("Starting server");
    let _server_handle = run_server(server);
    tracing::debug!("Starting client");

    let client = RpcClient::new(IrohNetConnector::new(client, server_node_addr, ALPN.into()));
    tracing::debug!("Starting benchmark");
    bench(client, 50000).await?;
    Ok(())
}

#[tokio::test]
async fn iroh_net_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_node_addr,
    } = Endpoints::new().await?;
    let _server_handle = run_server(server);
    let client_connection = IrohNetConnector::new(client, server_node_addr, ALPN.into());
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

    let client_endpoint = make_endpoint(SecretKey::generate(), ALPN).await?;

    let server_secret_key = SecretKey::generate();
    let server_node_id = server_secret_key.public();

    // create the RPC client
    let client_connection =
        transport::iroh_net::IrohNetConnector::<ComputeResponse, ComputeRequest>::new(
            client_endpoint.clone(),
            server_node_id,
            ALPN.into(),
        );
    let client = RpcClient::<
        ComputeService,
        transport::iroh_net::IrohNetConnector<ComputeResponse, ComputeRequest>,
    >::new(client_connection);

    // send a request. No server available so it should fail
    client.rpc(Sqr(4)).await.unwrap_err();

    let server_endpoint = make_endpoint(server_secret_key.clone(), ALPN).await?;

    // create the RPC Server
    let connection = transport::iroh_net::IrohNetListener::new(server_endpoint.clone())?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::spawn(ComputeService::server_bounded(server, 1));

    // wait a bit for connection due to Windows test failing on CI
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Passing the server node address directly to client endpoint to not depend
    // on a discovery service
    client_endpoint.add_node_addr(server_endpoint.node_addr().await?)?;

    // send the first request and wait for the response to ensure everything works as expected
    let SqrResponse(response) = client.rpc(Sqr(4)).await?;
    assert_eq!(response, 16);

    let server = server_handle.await??;
    drop(server);
    // wait for drop to free the socket
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    let server_endpoint = make_endpoint(server_secret_key.clone(), ALPN).await?;

    // make the server run again
    let connection = transport::iroh_net::IrohNetListener::new(server_endpoint.clone())?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::spawn(ComputeService::server_bounded(server, 5));

    // wait a bit for connection due to Windows test failing on CI
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // Passing the server node address directly to client endpoint to not depend
    // on a discovery service
    client_endpoint.add_node_addr(server_endpoint.node_addr().await?)?;

    // server is running, this should work
    let SqrResponse(response) = client.rpc(Sqr(3)).await?;
    assert_eq!(response, 9);
    server_handle.abort();
    Ok(())
}
