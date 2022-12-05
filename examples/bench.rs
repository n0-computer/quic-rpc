use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
    time::{Instant, Duration},
};

use anyhow::Context;
use futures::StreamExt;
use quic_rpc::{quinn::QuinnChannelTypes, rpc_service, ChannelTypes, RpcClient, RpcServer, yamux::YamuxChannelTypes};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use serde::{Deserialize, Serialize};
use tokio::{task::JoinHandle, net::{TcpSocket, TcpStream}, io::{AsyncReadExt, AsyncWriteExt}};
use tokio_util::compat::{TokioAsyncReadCompatExt, FuturesAsyncReadCompatExt};
use yamux::Control;

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
            println!("calling accept_one on server");
            let (req, chan) = s.accept_one().await?;
            use BenchRequest::*;
            let service = service.clone();
            let s = s.clone();
            tokio::task::spawn(async move {
                #[rustfmt::skip]
                match req {
                    BulkRequest(msg) => s.rpc(msg, chan, service, BenchService::bulk).await,
                    SmallRequest(msg) => s.rpc(msg, chan, service, BenchService::small).await,
                }?;
                anyhow::Ok(())
            });
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

/// Quic-rpc throughput benchmark
async fn quinn_bench() -> anyhow::Result<()> {
    type C = QuinnChannelTypes;
    type S = BenchService;
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    let server_handle = run_server(server);
    let client_connection = client.connect(server_addr, "localhost")?.await?;
    let client_connection =
        quic_rpc::quinn::Channel::<BenchResponse, BenchRequest>::new(client_connection);
    let client = RpcClient::<S, C>::new(client_connection);
    for i in 0..100 {
        client
            .rpc(BulkRequest(vec![i as u8; 1024 * 1024 * 4]))
            .await?;
    }
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

/// Raw quinn benchmark, without anything related to quic-rpc
async fn quinn_raw() -> anyhow::Result<()> {
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

fn config() -> yamux::Config {
    let mut c = yamux::Config::default();
    c
}

async fn yamux_bench() -> anyhow::Result<()> {
    type C = YamuxChannelTypes;
    type S = BenchService;
    let addr = "127.0.0.1:12120".parse()?;
    let server_handle = tokio::task::spawn(async move {
        let socket = TcpSocket::new_v4()?;
        println!("created socket");
        socket.bind(addr)?;
        println!("bound to socket");
        let listener = socket.listen(1024)?;
        println!("got listener");
        while let Ok((stream, _addr)) = listener.accept().await {
            println!("accepted one!");
            let connection =
                quic_rpc::yamux::Channel::<BenchRequest, BenchResponse>::new(stream.compat(), yamux::Config::default(), yamux::Mode::Server);
            let server = RpcServer::<S, C>::new(connection);
            BenchService::server(server).await?;
        }
        anyhow::Ok(())
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("calling connect {:?}", addr);
    let socket = TcpStream::connect(addr).await?;
    println!("connected to {:?}", addr);
    let client_connection =
    quic_rpc::yamux::Channel::<BenchResponse, BenchRequest>::new(socket.compat(), yamux::Config::default(), yamux::Mode::Client);
    tokio::task::spawn(client_connection.clone().consume_incoming_streams());
    let client = RpcClient::<S, C>::new(client_connection);
    for i in 0..100 {
        println!("sending message {}", i);
        client
            .rpc(BulkRequest(vec![i as u8; 1024 * 1024]))
            .await?;
    }
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

/// Raw quinn benchmark, without anything related to quic-rpc
async fn yamux_raw() -> anyhow::Result<()> {
    let addr = "127.0.0.1:12010".parse()?;
    let server_handle = tokio::task::spawn(async move {
        let socket = TcpSocket::new_v4()?;
        println!("S created socket");
        socket.bind(addr)?;
        println!("S bound to socket");
        let listener = socket.listen(1024)?;
        println!("S got listener");
        while let Ok((stream, addr)) = listener.accept().await {
            println!("S accepted one! {:?}", addr);
            let mut conn = yamux::Connection::new(stream.compat(), config(), yamux::Mode::Server);
            while let Some(x) = futures::future::poll_fn(|cx| conn.poll_next_inbound(cx)).await {
                tokio::task::spawn(async move {
                    println!("S got a substream or error {:?}", x);
                    let stream = x?;
                    println!("S got a substream {}", stream.id());
                    let mut stream = stream.compat();
                    let mut buffer = vec![0u8; 1024 * 1024];
                    loop {
                        println!("S reading {} bytes", buffer.len());
                        stream.read_exact(&mut buffer).await?;
                        println!("S writing {} bytes", buffer.len());
                        stream.write_all(&buffer).await?;
                    }
                    anyhow::Ok(())
                });
            }
        }
        anyhow::Ok(())
    });
    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("C calling connect {:?}", addr);
    let stream = TcpStream::connect(addr).await?;
    println!("C wrapping into yamux p:{:?} l:{:?}", stream.peer_addr(), stream.local_addr());
    let conn = yamux::Connection::new(stream.compat(), config(), yamux::Mode::Client);
    let (mut ctrl, conn) = Control::new(conn);
    tokio::task::spawn(conn.for_each(|r| {
        println!("x");
        r.unwrap();
        futures::future::ready(())
    }));
    println!("C opening substream");
    let stream = ctrl.open_stream().await?;
    println!("C opened substream {}", stream.id());
    let mut buffer = vec![0u8; 1024 * 1024];
    let mut stream = stream.compat();
    for i in 0..1000 {
        println!("C writing {} bytes", buffer.len());
        stream.write_all(&buffer).await?;
        println!("C reading {} bytes", buffer.len());
        stream.read_exact(&mut buffer).await?;
    }
    server_handle.abort();
    let _ = server_handle.await;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let t0 = Instant::now();
    // rt.block_on(yamux_bench())?;
    rt.block_on(yamux_bench())?;
    println!("Elapsed: {}", t0.elapsed().as_secs_f64());
    Ok(())
}
