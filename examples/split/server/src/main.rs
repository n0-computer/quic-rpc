use anyhow::Context;
use async_stream::stream;
use futures::stream::{Stream, StreamExt};
use quic_rpc::{server::run_server_loop, transport::QuinnChannelTypes};
use quinn::{Endpoint, ServerConfig};
use std::net::SocketAddr;
use std::sync::Arc;

use types::compute::*;

#[derive(Clone)]
pub struct Compute;

types::create_compute_dispatch!(Compute, dispatch_compute_request);

impl Compute {
    async fn square(self, req: Sqr) -> SqrResponse {
        SqrResponse(req.0 as u128 * req.0 as u128)
    }

    async fn sum(self, _req: Sum, updates: impl Stream<Item = SumUpdate>) -> SumResponse {
        let mut sum = 0u128;
        tokio::pin!(updates);
        while let Some(SumUpdate(n)) = updates.next().await {
            sum += n as u128;
        }
        SumResponse(sum)
    }

    fn fibonacci(self, req: Fibonacci) -> impl Stream<Item = FibonacciResponse> {
        let mut a = 0u128;
        let mut b = 1u128;
        let mut n = req.0;
        stream! {
            while n > 0 {
                yield FibonacciResponse(a);
                let c = a + b;
                a = b;
                b = c;
                n -= 1;
            }
        }
    }

    fn multiply(
        self,
        req: Multiply,
        updates: impl Stream<Item = MultiplyUpdate>,
    ) -> impl Stream<Item = MultiplyResponse> {
        let product = req.0 as u128;
        stream! {
            tokio::pin!(updates);
            while let Some(MultiplyUpdate(n)) = updates.next().await {
                yield MultiplyResponse(product * n as u128);
            }
        }
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let (server, _server_certs) = make_server_endpoint(server_addr)?;
    loop {
        let accept = server.accept().await.context("accept failed")?.await?;
        tokio::task::spawn(async move {
            let remote = accept.remote_address();
            eprintln!("new connection from {remote}");
            let connection = quic_rpc::transport::quinn::Channel::new(accept);
            let target = Compute;
            match run_server_loop(
                ComputeService,
                QuinnChannelTypes,
                connection,
                target,
                dispatch_compute_request,
            )
            .await
            {
                Ok(_) => eprintln!("connection to {remote} closed"),
                Err(err) => eprintln!("connection to {remote} closed: {err:?}"),
            }
        });
    }
}

fn make_server_endpoint(bind_addr: SocketAddr) -> anyhow::Result<(Endpoint, Vec<u8>)> {
    let (server_config, server_cert) = configure_server()?;
    let endpoint = Endpoint::server(server_config, bind_addr)?;
    Ok((endpoint, server_cert))
}

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
