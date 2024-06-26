#![cfg(feature = "quinn-transport")]
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use quic_rpc::{transport, RpcClient, RpcServer};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use tokio::task::JoinHandle;

mod math;
use math::*;
mod util;

/// Constructs a QUIC endpoint configured for use a client only.
///
/// ## Args
///
/// - server_certs: list of trusted certificates.
#[allow(unused)]
pub fn make_client_endpoint(
    bind_addr: SocketAddr,
    server_certs: &[&[u8]],
) -> anyhow::Result<Endpoint> {
    let client_cfg = configure_client(server_certs)?;
    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

/// Constructs a QUIC endpoint configured to listen for incoming connections on a certain address
/// and port.
///
/// ## Returns
///
/// - a stream of incoming QUIC connections
/// - server certificate serialized into DER format
#[allow(unused)]
pub fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<(Endpoint, Vec<u8>)> {
    let (server_config, server_cert) = configure_server()?;
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok((endpoint, server_cert))
}

/// Builds default quinn client config and trusts given certificates.
///
/// ## Args
///
/// - server_certs: a list of trusted certificates in DER format.
fn configure_client(server_certs: &[&[u8]]) -> anyhow::Result<ClientConfig> {
    let mut certs = rustls::RootCertStore::empty();
    for cert in server_certs {
        certs.add(&rustls::Certificate(cert.to_vec()))?;
    }
    Ok(ClientConfig::with_root_certificates(certs))
}

/// Returns default server configuration along with its certificate.
#[allow(clippy::field_reassign_with_default)] // https://github.com/rust-lang/rust-clippy/issues/6527
fn configure_server() -> anyhow::Result<(ServerConfig, Vec<u8>)> {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    let cert_der = cert.serialize_der()?;
    let priv_key = cert.serialize_private_key_der();
    let priv_key = rustls::PrivateKey(priv_key);
    let cert_chain = vec![rustls::Certificate(cert_der.clone())];

    let mut server_config = ServerConfig::with_single_cert(cert_chain, priv_key)?;
    Arc::get_mut(&mut server_config.transport)
        .unwrap()
        .max_concurrent_uni_streams(0_u8.into());

    Ok((server_config, cert_der))
}

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

fn run_server(server: quinn::Endpoint) -> JoinHandle<anyhow::Result<()>> {
    tokio::task::spawn(async move {
        let connection = transport::quinn::QuinnServerEndpoint::<ComputeService>::new(server)?;
        let server = RpcServer::new(connection);
        ComputeService::server(server).await?;
        anyhow::Ok(())
    })
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
    let server_handle = run_server(server);
    tracing::debug!("Starting client");
    let client = transport::quinn::QuinnConnection::<ComputeService>::new(
        client,
        server_addr,
        "localhost".into(),
    );
    let client = RpcClient::new(client);
    tracing::debug!("Starting benchmark");
    bench(client, 50000).await?;
    server_handle.abort();
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
    let server_handle = run_server(server);
    let client_connection = transport::quinn::QuinnConnection::<ComputeService>::new(
        client,
        server_addr,
        "localhost".into(),
    );
    smoke_test(client_connection).await?;
    server_handle.abort();
    Ok(())
}

/// Test that using the client after the server goes away and comes back behaves as if the server
/// had never gone away in the first place.
///
/// This is a regression test.
#[tokio::test]
async fn server_away_and_back() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    tracing::info!("Creating endpoints");

    let server_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 12347));
    let (server_config, server_cert) = configure_server()?;

    // create the RPC client
    let client = make_client_endpoint("0.0.0.0:0".parse()?, &[&server_cert])?;
    let client_connection = transport::quinn::QuinnConnection::<ComputeService>::new(
        client,
        server_addr,
        "localhost".into(),
    );
    let client = RpcClient::new(client_connection);

    // send a request. No server available so it should fail
    client.rpc(Sqr(4)).await.unwrap_err();

    // create the RPC Server
    let server = Endpoint::server(server_config.clone(), server_addr)?;
    let connection = transport::quinn::QuinnServerEndpoint::<ComputeService>::new(server)?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::task::spawn(ComputeService::server_bounded(server, 1));

    // send the first request and wait for the response to ensure everything works as expected
    let SqrResponse(response) = client.rpc(Sqr(4)).await.unwrap();
    assert_eq!(response, 16);

    let server = server_handle.await.unwrap().unwrap();
    drop(server);
    // wait for drop to free the socket
    tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

    // make the server run again
    let server = Endpoint::server(server_config, server_addr)?;
    let connection = transport::quinn::QuinnServerEndpoint::<ComputeService>::new(server)?;
    let server = RpcServer::new(connection);
    let server_handle = tokio::task::spawn(ComputeService::server_bounded(server, 5));

    // server is running, this should work
    let SqrResponse(response) = client.rpc(Sqr(3)).await.unwrap();
    assert_eq!(response, 9);

    server_handle.abort();
    Ok(())
}
