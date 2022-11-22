use futures::sink::SinkExt;
use futures::stream::StreamExt;
use quic_rpc::{quinn::QuinnChannelTypes, RpcClient};
use quinn::{ClientConfig, Endpoint};
use std::net::SocketAddr;
use std::sync::Arc;
use types::store::*;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let endpoint = make_insecure_client_endpoint("0.0.0.0:0".parse()?)?;
    let client = endpoint.connect(server_addr, "localhost")?.await?;
    let client = quic_rpc::quinn::Channel::new(client);
    let mut client = RpcClient::<StoreService, QuinnChannelTypes>::new(client);

    // a rpc call
    for i in 0..3 {
        println!("a rpc call [{i}]");
        let client = client.clone();
        tokio::task::spawn(async move {
            let res = client.rpc(Get([0u8; 32])).await;
            println!("rpc res [{i}]: {:?}", res);
        });
    }

    // server streaming call
    println!("a server streaming call");
    let mut s = client.server_streaming(GetFile([0u8; 32])).await?;
    while let Some(res) = s.next().await {
        println!("streaming res: {:?}", res);
    }

    // client streaming call
    println!("a client streaming call");
    let (mut send, recv) = client.client_streaming(PutFile).await?;
    tokio::task::spawn(async move {
        for i in 0..3 {
            send.send(PutFileUpdate(vec![i])).await.unwrap();
        }
    });
    let res = recv.await?;
    println!("client stremaing res: {:?}", res);

    // bidi streaming call
    println!("a bidi streaming call");
    let (mut send, mut recv) = client.bidi(ConvertFile).await?;
    tokio::task::spawn(async move {
        for i in 0..3 {
            send.send(ConvertFileUpdate(vec![i])).await.unwrap();
        }
    });
    while let Some(res) = recv.next().await {
        println!("bidi res: {:?}", res);
    }

    Ok(())
}

pub fn make_insecure_client_endpoint(bind_addr: SocketAddr) -> anyhow::Result<Endpoint> {
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
