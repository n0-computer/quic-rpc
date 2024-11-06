#![cfg(feature = "flume-transport")]
#![allow(non_local_definitions)]
mod math;
use math::*;
use quic_rpc::{
    server::{RpcChannel, RpcServerError},
    transport::flume,
    RpcClient, RpcServer, Service,
};

#[tokio::test]
async fn flume_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = flume::channel(1);

    let server = RpcServer::<ComputeService, _>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    let client = RpcClient::<ComputeService, _>::new(client);
    bench(client, 1000000).await?;
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::Accept(_)) => {}
        e => panic!("unexpected termination result {e:?}"),
    }
    Ok(())
}

#[tokio::test]
async fn flume_channel_mapped_bench() -> anyhow::Result<()> {
    use derive_more::{From, TryInto};
    use serde::{Deserialize, Serialize};

    tracing_subscriber::fmt::try_init().ok();

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum OuterRequest {
        Inner(InnerRequest),
    }
    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum InnerRequest {
        Compute(ComputeRequest),
    }
    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum OuterResponse {
        Inner(InnerResponse),
    }
    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    enum InnerResponse {
        Compute(ComputeResponse),
    }
    #[derive(Debug, Clone)]
    struct OuterService;
    impl Service for OuterService {
        type Req = OuterRequest;
        type Res = OuterResponse;
    }
    #[derive(Debug, Clone)]
    struct InnerService;
    impl Service for InnerService {
        type Req = InnerRequest;
        type Res = InnerResponse;
    }
    let (server, client) = flume::channel(1);

    let server = RpcServer::<OuterService, _>::new(server);
    let server_handle: tokio::task::JoinHandle<Result<(), RpcServerError<_>>> =
        tokio::task::spawn(async move {
            let service = ComputeService;
            loop {
                let (req, chan) = server.accept().await?.read_first().await?;
                let service = service.clone();
                tokio::spawn(async move {
                    let req: OuterRequest = req;
                    match req {
                        OuterRequest::Inner(InnerRequest::Compute(req)) => {
                            let chan: RpcChannel<InnerService, _> = chan.map();
                            let chan: RpcChannel<ComputeService, _> = chan.map();
                            ComputeService::handle_rpc_request(service, req, chan).await
                        }
                    }
                });
            }
        });

    let client = RpcClient::<OuterService, _>::new(client);
    let client: RpcClient<InnerService, _> = client.map();
    let client: RpcClient<ComputeService, _> = client.map();
    bench(client, 1000000).await?;
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::Accept(_)) => {}
        e => panic!("unexpected termination result {e:?}"),
    }
    Ok(())
}

/// simple happy path test for all 4 patterns
#[tokio::test]
async fn flume_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = flume::channel(1);

    let server = RpcServer::<ComputeService, _>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    smoke_test(client).await?;

    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::Accept(_)) => {}
        e => panic!("unexpected termination result {e:?}"),
    }
    Ok(())
}
