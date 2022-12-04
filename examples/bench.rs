use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::Instant,
};

use anyhow::Context;
use quic_rpc::{quinn::QuinnChannelTypes, rpc_service, ChannelTypes, RpcClient, RpcServer};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

#[derive(Debug, Serialize, Deserialize)]
pub struct BulkRequest(Vec<u8>);

#[derive(Debug, Serialize, Deserialize)]
pub struct BulkResponse(u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct SmallRequest(u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct SmallResponse(u64);

rpc_service! {
    Request = BenchRequest;
    Response = BenchResponse;
    Service = BenchService;
    CreateDispatch = _;
    CreateClient = _;

    Rpc bulk = BulkRequest, _ -> BulkResponse;
    Rpc get = SmallRequest, _ -> SmallResponse;
}

impl BenchService {
    async fn bulk(self, request: BulkRequest) -> BulkResponse {
        BulkResponse(request.0.len() as u64)
    }

    async fn small(self, request: SmallRequest) -> SmallResponse {
        SmallResponse(request.0)
    }

    async fn server<C: ChannelTypes>(service: RpcServer<Self, C>) -> anyhow::Result<()> {
        let s = service;
        let service = BenchService;
        loop {
            let (req, chan) = s.accept_one().await?;
            use BenchRequest::*;
            let service = service.clone();
            #[rustfmt::skip]
            match req {
                BulkRequest(msg) => s.rpc(msg, chan, service, BenchService::bulk).await,
                SmallRequest(msg) => s.rpc(msg, chan, service, BenchService::small).await,
            }?;
        }
        Ok(())
    }
}

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

pub fn make_endpoints() -> anyhow::Result<Endpoints> {
    let server_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 12345));
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
        let connection =
            quic_rpc::quinn::Channel::new(server.accept().await.context("accept failed")?.await?);
        let server = RpcServer::<BenchService, QuinnChannelTypes>::new(connection);
        BenchService::server(server).await?;
        anyhow::Ok(())
    })
}

async fn bench() -> anyhow::Result<()> {
    type C = QuinnChannelTypes;
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    let server_handle = run_server(server);
    let client_connection = client.connect(server_addr, "localhost")?.await?;
    let client_connection =
        quic_rpc::quinn::Channel::<BenchResponse, BenchRequest>::new(client_connection);
    let client = RpcClient::<BenchService, C>::new(client_connection);
    for i in 0..100 {
        client
            .rpc(BulkRequest(vec![i as u8; 1024 * 1024 * 4]))
            .await?;
    }
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

async fn bench_raw() -> anyhow::Result<()> {
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    let server = tokio::task::spawn(async move {
        while let Some(connecting) = server.accept().await {
            let connection = connecting.await?;
            let (mut send, mut recv) = connection.accept_bi().await?;
            let mut buffer = vec![0u8; 1024 * 1024];
            while let Some(n) = recv.read(&mut buffer).await? {
                send.write_all(&buffer[..n]).await?;
            }
        }
        anyhow::Ok(())
    });
    let client_connection = client.connect(server_addr, "localhost")?.await?;
    let (mut send, mut recv) = client_connection.open_bi().await?;
    let mut buffer = vec![0u8; 1024 * 1024];
    for i in 0..1000 {
        println!("{}", i);
        send.write_all(&buffer).await?;
        recv.read_exact(&mut buffer).await?;
    }
    server.abort();
    let _ = server.await;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let t0 = Instant::now();
    rt.block_on(bench_raw())?;
    println!("Elapsed: {}", t0.elapsed().as_secs_f64());
    Ok(())
}
