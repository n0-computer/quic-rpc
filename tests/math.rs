#![allow(dead_code)]
use async_stream::stream;
use derive_more::{From, TryInto};
use futures::{SinkExt, Stream, StreamExt, TryStreamExt};
use quic_rpc::{
    message::{BidiStreaming, ClientStreaming, Msg, RpcMsg, ServerStreaming},
    server::RpcServerError,
    ChannelTypes, RpcClient, RpcServer, Service,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Write},
    result,
};
use thousands::Separable;

/// compute the square of a number
#[derive(Debug, Serialize, Deserialize)]
pub struct Sqr(pub u64);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SqrResponse(pub u128);

/// sum a stream of numbers
#[derive(Debug, Serialize, Deserialize)]
pub struct Sum;

#[derive(Debug, Serialize, Deserialize)]
pub struct SumUpdate(pub u64);

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SumResponse(pub u128);

/// compute the fibonacci sequence as a stream
#[derive(Debug, Serialize, Deserialize)]
pub struct Fibonacci(pub u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct FibonacciResponse(pub u128);

/// multiply a stream of numbers, returning a stream
#[derive(Debug, Serialize, Deserialize)]
pub struct Multiply(pub u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct MultiplyUpdate(pub u64);

#[derive(Debug, Serialize, Deserialize)]
pub struct MultiplyResponse(pub u128);

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
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Serialize, Deserialize, From, TryInto)]
pub enum ComputeResponse {
    SqrResponse(SqrResponse),
    SumResponse(SumResponse),
    FibonacciResponse(FibonacciResponse),
    MultiplyResponse(MultiplyResponse),
}

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

    pub async fn server<C: ChannelTypes>(
        server: RpcServer<ComputeService, C>,
    ) -> result::Result<(), RpcServerError<C>> {
        let s = server;
        let service = ComputeService;
        loop {
            let (req, chan) = s.accept_one().await?;
            use ComputeRequest::*;
            let service = service.clone();
            #[rustfmt::skip]
            match req {
                Sqr(msg) => s.rpc(msg, chan, service, ComputeService::sqr).await,
                Sum(msg) => s.client_streaming(msg, chan, service, ComputeService::sum).await,
                Fibonacci(msg) => s.server_streaming(msg, chan, service, ComputeService::fibonacci).await,
                Multiply(msg) => s.bidi_streaming(msg, chan, service, ComputeService::multiply).await,
                SumUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                MultiplyUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
            }?;
        }
    }

    pub async fn server_par<C: ChannelTypes>(
        server: RpcServer<ComputeService, C>,
        parallelism: usize,
    ) -> result::Result<(), RpcServerError<C>> {
        let s = server.clone();
        let s2 = s.clone();
        let service = ComputeService;
        let request_stream = stream! {
            loop {
                yield s2.accept_one().await;
            }
        };
        let process_stream = request_stream.map(move |r| {
            let service = service.clone();
            let s = s.clone();
            async move {
                let (req, chan) = r?;
                use ComputeRequest::*;
                #[rustfmt::skip]
                match req {
                    Sqr(msg) => s.rpc(msg, chan, service, ComputeService::sqr).await,
                    Sum(msg) => s.client_streaming(msg, chan, service, ComputeService::sum).await,
                    Fibonacci(msg) => s.server_streaming(msg, chan, service, ComputeService::fibonacci).await,
                    Multiply(msg) => s.bidi_streaming(msg, chan, service, ComputeService::multiply).await,
                    SumUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                    MultiplyUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                }?;
                Ok::<_, RpcServerError<C>>(())
            }
        });
        process_stream
            .buffer_unordered(parallelism)
            .for_each(|x| async move {
                if let Err(e) = x {
                    eprintln!("error: {:?}", e);
                }
            })
            .await;
        Ok(())
    }
}

pub async fn smoke_test<C: ChannelTypes>(
    client: C::Channel<ComputeResponse, ComputeRequest>,
) -> anyhow::Result<()> {
    let client = RpcClient::<ComputeService, C>::new(client);
    // a rpc call
    let res = client.rpc(Sqr(1234)).await?;
    assert_eq!(res, SqrResponse(1522756));

    // client streaming call
    let (mut send, recv) = client.client_streaming(Sum).await?;
    tokio::task::spawn(async move {
        for i in 1..=3 {
            send.send(SumUpdate(i)).await?;
        }
        Ok::<_, C::SendError>(())
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
        Ok::<_, C::SendError>(())
    });
    let res = recv.map_ok(|x| x.0).try_collect::<Vec<_>>().await?;
    assert_eq!(res, vec![2, 4, 6]);
    Ok(())
}

fn clear_line() {
    print!("\r{}\r", " ".repeat(80));
}

pub async fn bench<C: ChannelTypes>(
    client: RpcClient<ComputeService, C>,
    n: u64,
) -> anyhow::Result<()>
where
    C::SendError: std::error::Error,
{
    // individual RPCs
    {
        let mut sum = 0;
        let t0 = std::time::Instant::now();
        for i in 0..n {
            sum += client.rpc(Sqr(i)).await?.0;
            if i % 10000 == 0 {
                print!(".");
                io::stdout().flush()?;
            }
        }
        let rps = ((n as f64) / t0.elapsed().as_secs_f64()).round();
        assert_eq!(sum, sum_of_squares(n));
        clear_line();
        println!("RPC seq {} rps", rps.separate_with_underscores(),);
    }
    // parallel RPCs
    {
        let t0 = std::time::Instant::now();
        let reqs = futures::stream::iter((0..n).map(Sqr));
        let resp = reqs
            .map(|x| {
                let client = client.clone();
                async move {
                    let res = client.rpc(x).await?.0;
                    anyhow::Ok(res)
                }
            })
            .buffer_unordered(32)
            .try_collect::<Vec<_>>()
            .await?;
        let sum = resp.into_iter().sum::<u128>();
        let rps = ((n as f64) / t0.elapsed().as_secs_f64()).round();
        assert_eq!(sum, sum_of_squares(n));
        clear_line();
        println!("RPC par {} rps", rps.separate_with_underscores(),);
    }
    // sequential streaming
    {
        let t0 = std::time::Instant::now();
        let (send, recv) = client.bidi(Multiply(2)).await?;
        let handle = tokio::task::spawn(async move {
            let requests = futures::stream::iter((0..n).map(MultiplyUpdate));
            requests.map(Ok).forward(send).await?;
            anyhow::Result::<()>::Ok(())
        });
        let mut sum = 0;
        tokio::pin!(recv);
        let mut i = 0;
        while let Some(res) = recv.next().await {
            sum += res?.0;
            if i % 10000 == 0 {
                print!(".");
                io::stdout().flush()?;
            }
            i += 1;
        }
        assert_eq!(sum, (0..n as u128).map(|x| x * 2).sum());
        let rps = ((n as f64) / t0.elapsed().as_secs_f64()).round();
        clear_line();
        println!("bidi seq {} rps", rps.separate_with_underscores(),);

        handle.await??;
    }
    Ok(())
}

fn sum_of_squares(n: u64) -> u128 {
    (0..n).map(|x| (x * x) as u128).sum()
}
