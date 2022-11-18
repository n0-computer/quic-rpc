use async_stream::stream;
use derive_more::{From, TryInto};
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use quic_rpc::{
    mem::{self, AcceptBiError, MemChannelTypes},
    sugar::{
        BidiStreaming, ClientChannel, ClientStreaming, Msg, RpcMsg, RpcServerError, ServerChannel,
        ServerStreaming,
    },
    Service,
};
use serde::{Deserialize, Serialize};
use std::result;

/// compute the square of a number
#[derive(Debug, Serialize, Deserialize)]
pub struct Sqr(u64);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqrResponse(u128);

/// sum a stream of numbers
#[derive(Debug, Serialize, Deserialize)]
pub struct Sum;

#[derive(Debug, Serialize, Deserialize)]
pub struct SumUpdate(u64);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SumResponse(u128);

/// compute the fibonacci sequence as a stream
#[derive(Debug, Serialize, Deserialize)]
pub struct Fibonacci(u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct FibonacciResponse(u128);

/// multiply a stream of numbers, returning a stream
#[derive(Debug, Serialize, Deserialize)]
pub struct Multiply(u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct MultiplyUpdate(u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct MultiplyResponse(u128);

/// request enum
#[derive(Debug, Serialize, Deserialize, From, TryInto)]
pub enum ComputeRequest {
    Sqr(Sqr),
    Sum(Sum),
    SumUpdate(SumUpdate),
    Fibonacci(Fibonacci),
    Multiply(Multiply),
    MultiplyUpdate(MultiplyUpdate),
}

/// response enum
#[derive(Debug, Serialize, Deserialize, From, TryInto)]
pub enum ComputeResponse {
    SqrResponse(SqrResponse),
    SumResponse(SumResponse),
    FibonacciResponse(FibonacciResponse),
    MultiplyResponse(MultiplyResponse),
}

#[derive(Debug, Clone)]
struct ComputeService;

impl Service for ComputeService {
    type Req = ComputeRequest;
    type Res = ComputeResponse;
}

impl RpcMsg<ComputeService> for Sqr {
    type Response = SqrResponse;
}

impl Msg<ComputeService> for Sum {
    type Response = SumResponse;
    type Update = SumUpdate;
    type Pattern = ClientStreaming;
}

impl Msg<ComputeService> for Fibonacci {
    type Response = FibonacciResponse;
    type Update = Self;
    type Pattern = ServerStreaming;
}

impl Msg<ComputeService> for Multiply {
    type Response = MultiplyResponse;
    type Update = MultiplyUpdate;
    type Pattern = BidiStreaming;
}

impl ComputeService {
    async fn sqr(self, req: Sqr) -> SqrResponse {
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

    async fn server(
        server: ServerChannel<ComputeService, MemChannelTypes>,
    ) -> result::Result<(), RpcServerError<MemChannelTypes>> {
        let mut s = server;
        let service = ComputeService;
        loop {
            let (req, chan) = s.accept_one().await?;
            use ComputeRequest::*;
            let service = service.clone();
            match req {
                Sqr(msg) => s.rpc(msg, chan, service, ComputeService::sqr).await,
                Sum(msg) => {
                    s.client_streaming(msg, chan, service, ComputeService::sum)
                        .await
                }
                Fibonacci(msg) => {
                    s.server_streaming(msg, chan, service, ComputeService::fibonacci)
                        .await
                }
                Multiply(msg) => {
                    s.bidi_streaming(msg, chan, service, ComputeService::multiply)
                        .await
                }
                SumUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                MultiplyUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
            }?;
        }
    }
}

/// simple happy path test for all 4 patterns
#[tokio::test]
async fn smoke() -> anyhow::Result<()> {
    let (client, server) = mem::connection::<ComputeResponse, ComputeRequest>(1);
    let mut client = ClientChannel::<ComputeService, MemChannelTypes>::new(client);
    let server = ServerChannel::<ComputeService, MemChannelTypes>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));

    // a rpc call
    let res = client.rpc(Sqr(1234)).await?;
    assert_eq!(res, SqrResponse(1522756));

    // client streaming call
    let (mut send, recv) = client.client_streaming(Sum).await?;
    tokio::task::spawn(async move {
        for i in 1..=3 {
            send.send(SumUpdate(i)).await?;
        }
        anyhow::Ok(())
    });
    let res = recv.await?;
    assert_eq!(res, SumResponse(6));

    // server streaming call
    let s = client.server_streaming(Fibonacci(10)).await?;
    let res = s.map_ok(|x| x.0).try_collect::<Vec<_>>().await?;
    assert_eq!(res, vec![0, 1, 1, 2, 3, 5, 8, 13, 21, 34]);

    // bidi streaming call
    let (mut send, recv) = client.bidi(Multiply(2)).await?;
    tokio::task::spawn(async move {
        for i in 1..=3 {
            send.send(MultiplyUpdate(i)).await?;
        }
        anyhow::Ok(())
    });
    let res = recv.map_ok(|x| x.0).try_collect::<Vec<_>>().await?;
    assert_eq!(res, vec![2, 4, 6]);

    // dropping the client will cause the server to terminate
    drop(client);
    match server_handle.await? {
        Err(RpcServerError::AcceptBiError(AcceptBiError::SenderDropped)) => (),
        e => panic!("server should have terminated with an error {:?}", e),
    }
    Ok(())
}
