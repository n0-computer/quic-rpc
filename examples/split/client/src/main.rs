use futures::sink::SinkExt;
use futures::stream::StreamExt;
use quic_rpc::RpcClient;
use quinn::{ClientConfig, Endpoint};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use types::compute::*;

types::create_compute_client!(ComputeClient);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let endpoint = make_insecure_client_endpoint("0.0.0.0:0".parse()?)?;
    let client = quic_rpc::transport::quinn::QuinnClientChannel::new(
        endpoint,
        server_addr,
        "localhost".to_string(),
    );
    let client = RpcClient::<ComputeService, _>::new(client);
    let mut client = ComputeClient(client);

    // a rpc call
    for i in 0..3 {
        let mut client = client.clone();
        tokio::task::spawn(async move {
            println!("rpc call: square([{i}])");
            let res = client.square(Sqr(i)).await;
            println!("rpc res: square({i}) = {:?}", res.unwrap());
        });
    }

    // client streaming call
    println!("client streaming call: sum()");
    let (mut send, recv) = client.sum(Sum).await?;
    tokio::task::spawn(async move {
        for i in 2..4 {
            println!("client streaming update: {i}");
            send.send(SumUpdate(i)).await.unwrap();
        }
    });
    let res = recv.await?;
    println!("client streaming res: {:?}", res);

    // server streaming call
    println!("server streaming call: fibonacci(10)");
    let mut s = client.fibonacci(Fibonacci(10)).await?;
    while let Some(res) = s.next().await {
        println!("server streaming res: {:?}", res?);
    }

    // bidi streaming call
    println!("bidi streaming call: multiply(2)");
    let (mut send, mut recv) = client.multiply(Multiply(2)).await?;
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

pub fn make_insecure_client_endpoint(bind_addr: SocketAddr) -> io::Result<Endpoint> {
    let crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(SkipServerVerification))
        .with_no_client_auth();

    let client_cfg = ClientConfig::new(Arc::new(crypto));
    let mut endpoint = Endpoint::client(bind_addr)?;
    endpoint.set_default_client_config(client_cfg);
    Ok(endpoint)
}

struct SkipServerVerification;
impl rustls::client::ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}
