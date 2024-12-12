#![cfg(feature = "quinn-transport")]
#![cfg(feature = "test-utils")]
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};

use quic_rpc::{
    transport::{
        self,
        quinn::{
            configure_server, make_client_endpoint, make_server_endpoint, QuinnConnector,
            QuinnListener,
        },
    },
    RpcClient, RpcServer,
};
use quinn::Endpoint;

mod math;
use math::*;
use testresult::TestResult;
use tokio_util::task::AbortOnDropHandle;
mod util;

pub struct Endpoints {
    client: Endpoint,
    server: Endpoint,
    server_addr: SocketAddr,
}

pub fn make_endpoints(port: u16) -> anyhow::Result<Endpoints> {
    let server_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port));
    let (server, server_certs) = make_server_endpoint(server_addr)?;
    let client = make_client_endpoint("0.0.0.0:0".parse()?, &[&server_certs])?;
    Ok(Endpoints {
        client,
        server,
        server_addr,
    })
}

fn run_server(server: quinn::Endpoint) -> AbortOnDropHandle<()> {
    let listener = QuinnListener::new(server).unwrap();
    let listener = RpcServer::new(listener);
    ComputeService::server(listener)
}

// #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[tokio::test]
async fn quinn_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints(12345)?;
    tracing::debug!("Starting server");
    let _server_handle = run_server(server);
    tracing::debug!("Starting client");
    let client = QuinnConnector::new(client, server_addr, "localhost".into());
    let client = RpcClient::new(client);
    tracing::debug!("Starting benchmark");
    bench(client, 50000).await?;
    Ok(())
}

#[tokio::test]
async fn quinn_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints(12346)?;
    let _server_handle = run_server(server);
    let client_connection =
        transport::quinn::QuinnConnector::new(client, server_addr, "localhost".into());
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

    let server_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 12347));
    let (server_config, server_cert) = configure_server()?;

    // create the RPC client
    let client = make_client_endpoint("0.0.0.0:0".parse()?, &[&server_cert])?;
    let client_connection =
        transport::quinn::QuinnConnector::new(client, server_addr, "localhost".into());
    let client = RpcClient::new(client_connection);

    // send a request. No server available so it should fail
    client.rpc(Sqr(4)).await.unwrap_err();

    // create the RPC Server
    let server = Endpoint::server(server_config.clone(), server_addr)?;
    let connection = transport::quinn::QuinnListener::new(server)?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::task::spawn(ComputeService::server_bounded(server, 1));

    // send the first request and wait for the response to ensure everything works as expected
    let SqrResponse(response) = client.rpc(Sqr(4)).await?;
    assert_eq!(response, 16);

    let server = server_handle.await??;
    drop(server);
    // wait for drop to free the socket
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // make the server run again
    let server = Endpoint::server(server_config, server_addr)?;
    let connection = transport::quinn::QuinnListener::new(server)?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::task::spawn(ComputeService::server_bounded(server, 5));

    // server is running, this should work
    let SqrResponse(response) = client.rpc(Sqr(3)).await?;
    assert_eq!(response, 9);
    server_handle.abort();
    Ok(())
}
