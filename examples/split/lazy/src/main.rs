//! Lazy client example
//!
//! This opens a RpcClient in lazy mode and tries to perform some PRC calls in a loop.
//!
//! To try this out, start and stop the server in a separate terminal. You should see
//! the client reconnecting to the server after some time.
use quic_rpc::channel_factory::LazyChannelFactory;
use quic_rpc::{transport::QuinnChannelTypes, RpcClient};
use quinn::{ClientConfig, Endpoint};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use types::compute::*;

types::create_compute_client!(ComputeClient);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let bind_addr: SocketAddr = "0.0.0.0:0".parse()?;
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let server_name = "localhost";
    let factory = LazyChannelFactory::<ComputeResponse, ComputeRequest, QuinnChannelTypes, _>::lazy(
        move || async move {
            println!("opening endpoint on {}", bind_addr);
            let endpoint = make_insecure_client_endpoint(bind_addr).map_err(|e| dbg!(e))?;
            println!("connecting to {} with {}", server_addr, server_name);
            let connection = endpoint
                .connect(server_addr, server_name)
                .map_err(|e| dbg!(e))?
                .await
                .map_err(|e| dbg!(e))?;
            println!("connected. creating channel");
            let client = quic_rpc::transport::quinn::Channel::new(connection);
            Ok(client)
        },
    );
    let client = RpcClient::<ComputeService, QuinnChannelTypes>::from_factory(Arc::new(factory));

    for i in 0..100000 {
        match client.rpc(Sqr(i)).await {
            Ok(res) => {
                println!("{} Got result {}", i, res.0);
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => {
                println!("{} Got error: {}", i, e);
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
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
