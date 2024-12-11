use std::net::SocketAddr;

use async_stream::stream;
use futures::stream::{Stream, StreamExt};
use quic_rpc::{
    server::run_server_loop,
    transport::quinn::{make_server_endpoint, QuinnListener},
};
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
    tracing_subscriber::fmt::init();
    let server_addr: SocketAddr = "127.0.0.1:12345".parse()?;
    let (server, _server_certs) = make_server_endpoint(server_addr)?;
    let channel = QuinnListener::new(server)?;
    let target = Compute;
    run_server_loop(
        ComputeService,
        channel.clone(),
        target,
        dispatch_compute_request,
    )
    .await?;
    Ok(())
}
