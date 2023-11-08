#![cfg(feature = "interprocess-transport")]
use std::{
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    sync::Arc,
};

use futures::{io::BufReader, AsyncBufReadExt, AsyncWriteExt as _};
use quic_rpc::{
    transport::interprocess::{new_socket_name, tokio_io_endpoint},
    RpcClient, RpcServer,
};
use quinn::{ClientConfig, Endpoint, ServerConfig};
use tokio::task::JoinHandle;
use tokio_util::compat::{FuturesAsyncReadCompatExt, FuturesAsyncWriteCompatExt};

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

pub fn make_endpoints() -> anyhow::Result<Endpoints> {
    let server_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1));
    let client_addr: SocketAddr = SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2));
    let (server_config, server_cert) = configure_server()?;
    let client_config = configure_client(&[&server_cert])?;
    let (server, mut client) = quic_rpc::transport::quinn_flume_socket::endpoint_pair(
        server_addr,
        client_addr,
        server_config,
    )
    .unwrap();
    client.set_default_client_config(client_config);
    Ok(Endpoints {
        client,
        server,
        server_addr,
    })
}

fn run_server(server: quinn::Endpoint) -> JoinHandle<anyhow::Result<()>> {
    tokio::task::spawn(async move {
        let connection = quic_rpc::transport::quinn::QuinnServerEndpoint::new(server)?;
        let server = RpcServer::<ComputeService, _>::new(connection);
        ComputeService::server(server).await?;
        anyhow::Ok(())
    })
}

#[tokio::test]
async fn quinn_flume_socket_raw() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    tracing::debug!("Starting server");
    let server = tokio::spawn(async move {
        tracing::info!("server started");
        while let Some(connecting) = server.accept().await {
            tracing::info!("server accepted connection");
            let connection = connecting.await?;
            while let Ok((mut send, mut recv)) = connection.accept_bi().await {
                tracing::info!("server accepted bidi stream");
                tokio::io::copy(&mut recv, &mut send).await?;
                tracing::info!("server done with bidi stream");
            }
        }
        anyhow::Ok(())
    });
    let client = tokio::spawn(async move {
        let conn = client.connect(server_addr, "localhost")?.await?;
        let (mut send, mut recv) = conn.open_bi().await?;
        tracing::info!("client connected");
        tokio::spawn(async move {
            tracing::info!("outputting data from server");
            tokio::io::copy(&mut recv, &mut tokio::io::stdout()).await?;
            tracing::info!("outputting data from server done");
            anyhow::Ok(())
        });
        tracing::info!("sending data to be echoed");
        let test = vec![0u8; 1024];
        send.write_all(&test).await?;
        tracing::info!("sending data done");
        anyhow::Ok(())
    });
    server.await??;
    client.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quinn_flume_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    tracing::debug!("Starting server");
    let server_handle = run_server(server);
    tracing::debug!("Starting client");
    let client =
        quic_rpc::transport::quinn::QuinnConnection::new(client, server_addr, "localhost".into());
    let client = RpcClient::<ComputeService, _>::new(client);
    tracing::debug!("Starting benchmark");
    bench(client, 50000).await?;
    server_handle.abort();
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quinn_flume_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let Endpoints {
        client,
        server,
        server_addr,
    } = make_endpoints()?;
    let server_handle = run_server(server);
    let client_connection =
        quic_rpc::transport::quinn::QuinnConnection::new(client, server_addr, "localhost".into());
    smoke_test(client_connection).await?;
    server_handle.abort();
    Ok(())
}

/// Basic test of the interprocess crate.
/// Just sends a message from client to server and back.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interprocess_accept_connect_raw() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    use interprocess::local_socket::tokio::*;
    let dir = tempfile::tempdir()?;
    let socket_name = new_socket_name(dir.path(), "interprocess");
    let socket = LocalSocketListener::bind(socket_name.clone())?;
    let socket_name_2 = socket_name.clone();
    let server = tokio::spawn(async move {
        tracing::info!("server: spawn");
        let stream = socket.accept().await?;
        tracing::info!("server: accepted");
        let (r, mut w) = stream.into_split();

        let mut buffer = String::new();
        let mut reader = BufReader::new(r);

        tracing::info!("server: reading");
        reader.read_line(&mut buffer).await?;
        tracing::info!("server: read: {:?}", buffer);
        w.write_all(buffer.as_bytes()).await?;

        anyhow::Ok(())
    });
    let client = tokio::spawn(async move {
        tracing::info!("client: spawned");
        let stream = LocalSocketStream::connect(socket_name_2.clone()).await?;
        tracing::info!("client: connected");
        let (r, mut w) = stream.into_split();

        tracing::info!("client: writting");
        w.write_all(b"hello").await?;
        w.write_all(b"world").await?;
        w.write_all(b"\n").await?;
        tracing::info!("client: written");
        let validator = tokio::spawn(async move {
            tracing::info!("client: reading response");
            let mut buffer = String::new();
            let mut reader = BufReader::new(r);
            reader.read_line(&mut buffer).await?;
            let pass = buffer == "helloworld\n";

            anyhow::Ok(pass)
        });
        drop(w);
        validator.await?
    });
    server.await??;
    assert!(client.await??, "echo test failed");
    Ok(())
}

/// Test of quinn on top of interprocess.
/// Just sends a message from client to server and back.
#[tokio::test]
async fn interprocess_quinn_accept_connect_raw() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    use interprocess::local_socket::tokio::*;
    let (server_config, server_certs) = configure_server()?;
    let client_config = configure_client(&[&server_certs])?;

    let dir = tempfile::tempdir()?;
    let socket_name = new_socket_name(dir.path(), "interprocess");
    let socket = LocalSocketListener::bind(socket_name.clone())?;
    let local = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into();
    let remote = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2).into();
    let server = tokio::spawn(async move {
        let stream = socket.accept().await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (endpoint, _s, _r) = tokio_io_endpoint(r, w, remote, local, Some(server_config))?;
        tracing::debug!("server accepting connection");
        let connection = endpoint.accept().await.unwrap().await?;
        tracing::debug!("server accepted connection");
        let (mut send, mut recv) = connection.accept_bi().await?;
        tracing::debug!("server accepted bi stream");
        tracing::debug!("server copying");
        let n = tokio::io::copy(&mut recv, &mut send).await?;
        tracing::debug!("server done copying {} bytes", n);
        // need to keep the connection alive until the client is done
        anyhow::Ok(connection)
    });
    let client = tokio::spawn(async move {
        let stream = LocalSocketStream::connect(socket_name).await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (mut endpoint, _s, _r) = tokio_io_endpoint(r, w, local, remote, None)?;
        endpoint.set_default_client_config(client_config);
        tracing::debug!("client connecting to server at {} using localhost", remote);
        let connection = endpoint.connect(remote, "localhost")?.await?;
        tracing::debug!("client got connection");
        let (mut send, mut recv) = connection.open_bi().await?;
        tracing::debug!("client got bi stream");
        let validator = tokio::spawn(async move {
            let mut out = Vec::<u8>::new();
            tracing::debug!("client validator reading data");
            tokio::io::copy(&mut recv, &mut out).await.ok();
            tracing::debug!("client validator done reading data {}", out.len());
            let pass = out == b"helloworld";
            anyhow::Ok(pass)
        });
        tracing::debug!("client sending data");
        send.write_all(b"hello").await?;
        send.write_all(b"world").await?;
        send.finish().await?;
        tracing::debug!("client finished sending data");
        drop(send);
        validator.await?
    });
    let connection = server.await??;
    assert!(client.await??, "echo test failed");
    drop(connection);
    Ok(())
}

/// Smoke test of quinn on top of interprocess.
#[tokio::test]
async fn interprocess_quinn_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    use interprocess::local_socket::tokio::*;
    let (server_config, server_certs) = configure_server()?;
    let client_config = configure_client(&[&server_certs])?;

    let dir = tempfile::tempdir()?;
    let socket_name = new_socket_name(dir.path(), "interprocess");
    let socket = LocalSocketListener::bind(socket_name.clone())?;
    let local = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into();
    let remote = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2).into();
    let server = tokio::spawn(async move {
        let stream = socket.accept().await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (endpoint, _s, _r) = tokio_io_endpoint(r, w, remote, local, Some(server_config))?;
        // run rpc server on endpoint
        tracing::debug!("creating test rpc server");
        let _server_task = run_server(endpoint);
        anyhow::Ok(())
    });
    let client = tokio::spawn(async move {
        let stream = LocalSocketStream::connect(socket_name).await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (mut endpoint, _s, _r) = tokio_io_endpoint(r, w, local, remote, None)?;
        endpoint.set_default_client_config(client_config);
        tracing::debug!(
            "creating test rpc client, connecting to server at {} using localhost",
            remote
        );
        let client: quic_rpc::transport::quinn::QuinnConnection<ComputeResponse, ComputeRequest> =
            quic_rpc::transport::quinn::QuinnConnection::new(endpoint, remote, "localhost".into());
        smoke_test(client).await?;
        anyhow::Ok(())
    });
    server.await??;
    client.await??;
    Ok(())
}

/// Bench test of quinn on top of interprocess.
#[tokio::test]
async fn interprocess_quinn_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    use interprocess::local_socket::tokio::*;
    let (server_config, server_certs) = configure_server()?;
    let client_config = configure_client(&[&server_certs])?;

    let dir = tempfile::tempdir()?;
    let socket_name = new_socket_name(dir.path(), "interprocess");
    let socket = LocalSocketListener::bind(socket_name.clone())?;
    let local = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 1).into();
    let remote = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 2).into();
    let server = tokio::spawn(async move {
        let stream = socket.accept().await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (endpoint, _s, _r) = tokio_io_endpoint(r, w, remote, local, Some(server_config))?;
        // run rpc server on endpoint
        tracing::debug!("creating test rpc server");
        let _server_task = run_server(endpoint);
        anyhow::Ok(())
    });
    let client = tokio::spawn(async move {
        let stream = LocalSocketStream::connect(socket_name).await?;
        let (r, w) = stream.into_split();
        let r = r.compat();
        let w = w.compat_write();
        let (mut endpoint, _s, _r) = tokio_io_endpoint(r, w, local, remote, None)?;
        endpoint.set_default_client_config(client_config);
        tracing::debug!(
            "creating test rpc client, connecting to server at {} using localhost",
            remote
        );
        let client: quic_rpc::transport::quinn::QuinnConnection<ComputeResponse, ComputeRequest> =
            quic_rpc::transport::quinn::QuinnConnection::new(endpoint, remote, "localhost".into());
        let client = RpcClient::new(client);
        bench(client, 50000).await?;
        anyhow::Ok(())
    });
    server.await??;
    client.await??;
    Ok(())
}
