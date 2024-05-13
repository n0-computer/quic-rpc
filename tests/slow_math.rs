#![cfg(any(
    feature = "flume-transport",
    feature = "hyper-transport",
    feature = "quinn-transport"
))]
mod math;
use std::result;

use async_stream::stream;
use futures_lite::{Stream, StreamExt};
use math::*;
use quic_rpc::{
    message::{
        BidiStreaming, BidiStreamingMsg, ClientStreaming, ClientStreamingMsg, Msg, RpcMsg,
        ServerStreaming, ServerStreamingMsg,
    },
    server::RpcServerError,
    RpcServer, Service, ServiceEndpoint,
};

#[derive(Debug, Clone)]
pub struct ComputeService;

impl Service for ComputeService {
    type Req = ComputeRequest;
    type Res = ComputeResponse;
}

impl RpcMsg<ComputeService> for Sqr {
    type Response = SqrResponse;
}

impl Msg<ComputeService> for Sum {
    type Pattern = ClientStreaming;
}

impl ClientStreamingMsg<ComputeService> for Sum {
    type Update = SumUpdate;
    type Response = SumResponse;
}

impl Msg<ComputeService> for Fibonacci {
    type Pattern = ServerStreaming;
}

impl ServerStreamingMsg<ComputeService> for Fibonacci {
    type Response = FibonacciResponse;
}

impl Msg<ComputeService> for Multiply {
    type Pattern = BidiStreaming;
}

impl BidiStreamingMsg<ComputeService> for Multiply {
    type Update = MultiplyUpdate;
    type Response = MultiplyResponse;
}

async fn sleep_ms(ms: u64) {
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
}

impl ComputeService {
    async fn sqr(self, req: Sqr) -> SqrResponse {
        sleep_ms(10000).await;
        SqrResponse(req.0 as u128 * req.0 as u128)
    }

    async fn sum(self, _req: Sum, updates: impl Stream<Item = SumUpdate>) -> SumResponse {
        let mut sum = 0u128;
        tokio::pin!(updates);
        while let Some(SumUpdate(n)) = updates.next().await {
            sleep_ms(100).await;
            sum += n as u128;
        }
        SumResponse(sum)
    }

    fn fibonacci(self, req: Fibonacci) -> impl Stream<Item = FibonacciResponse> {
        let mut a = 0u128;
        let mut b = 1u128;
        let mut n = req.0;
        stream! {
            sleep_ms(100).await;
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
                sleep_ms(100).await;
                yield MultiplyResponse(product * n as u128);
            }
        }
    }

    pub async fn server<C: ServiceEndpoint<ComputeService>>(
        server: RpcServer<ComputeService, C>,
    ) -> result::Result<(), RpcServerError<C>> {
        let s = server;
        let service = ComputeService;
        loop {
            let (req, chan) = s.accept().await?;
            use ComputeRequest::*;
            let service = service.clone();
            #[rustfmt::skip]
            match req {
                Sqr(msg) => chan.rpc(msg, service, ComputeService::sqr).await,
                Sum(msg) => chan.client_streaming(msg, service, ComputeService::sum).await,
                Fibonacci(msg) => chan.server_streaming(msg, service, ComputeService::fibonacci).await,
                Multiply(msg) => chan.bidi_streaming(msg, service, ComputeService::multiply).await,
                SumUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                MultiplyUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
            }?;
        }
    }
}
