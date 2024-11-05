#![allow(unknown_lints, non_local_definitions)]

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Result;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use quic_rpc::transport::quinn::QuinnConnector;
use quic_rpc::RpcClient;
use quinn::crypto::rustls::QuicClientConfig;
use quinn::{ClientConfig, Endpoint};
use types::compute::*;

// types::create_compute_client!(ComputeClient);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let endpoint = make_insecure_client_endpoint("0.0.0.0:0".parse()?)?;
    let client = QuinnConnector::new(endpoint, server_addr, "localhost".to_string());
    let client = RpcClient::new(client);
    // let mut client = ComputeClient(client);

    // a rpc call
    for i in 0..3 {
        let client = client.clone();
        tokio::task::spawn(async move {
            println!("rpc call: square([{i}])");
            let res = client.rpc(Sqr(i)).await;
            println!("rpc res: square({i}) = {:?}", res.unwrap());
        });
    }

    // client streaming call
    println!("client streaming call: sum()");
    let (mut send, recv) = client.client_streaming(Sum).await?;
    tokio::task::spawn(async move {
        for i in 2..4 {
            println!("client streaming update: {i}");
            send.send(SumUpdate(i)).await.unwrap();
        }
    });
    let res = recv.await?;
    println!("client streaming res: {res:?}");

    // server streaming call
    println!("server streaming call: fibonacci(10)");
    let mut s = client.server_streaming(Fibonacci(10)).await?;
    while let Some(res) = s.next().await {
        println!("server streaming res: {:?}", res?);
    }

    // bidi streaming call
    println!("bidi streaming call: multiply(2)");
    let (mut send, mut recv) = client.bidi(Multiply(2)).await?;
    tokio::task::spawn(async move {
        for i in 1..3 {
            println!("bidi streaming update: {i}");
            send.send(MultiplyUpdate(i)).await.unwrap();
        }
    });
    while let Some(res) = recv.next().await {
        println!("bidi streaming res: {:?}", res?);
    }

    Ok(())
}

pub fn make_insecure_client_endpoint(bind_addr: SocketAddr) -> Result<Endpoint> {
    let crypto = rustls::ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let client_cfg = QuicClientConfig::try_from(crypto)?;
    let client_cfg = ClientConfig::new(Arc::new(client_cfg));
    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

#[derive(Debug)]
struct SkipServerVerification;

impl rustls::client::danger::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls::pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![]
    }
}
