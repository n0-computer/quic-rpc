#![cfg(feature = "tokio-mpsc-transport")]
#![allow(non_local_definitions)]
mod math;
use math::*;
use quic_rpc::{
    server::{RpcChannel, RpcServerError},
    transport::tokio_mpsc,
    RpcClient, RpcServer, Service,
};

#[tokio::test]
async fn async_channel_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = tokio_mpsc::connection::<ComputeService>(1);

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
async fn async_channel_channel_mapped_bench() -> anyhow::Result<()> {
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
    let (server, client) = tokio_mpsc::connection::<OuterService>(1);

    let server = RpcServer::new(server);
    let server_handle: tokio::task::JoinHandle<Result<(), RpcServerError<_>>> =
        tokio::task::spawn(async move {
            let service = ComputeService;
            loop {
                let (req, chan) = server.accept().await?;
                let service = service.clone();
                tokio::spawn(async move {
                    let req: OuterRequest = req;
                    match req {
                        OuterRequest::Inner(InnerRequest::Compute(req)) => {
                            let chan: RpcChannel<OuterService, _, InnerService> = chan.map();
                            let chan: RpcChannel<OuterService, _, ComputeService> = chan.map();
                            ComputeService::handle_rpc_request(service, req, chan).await
                        }
                    }
                });
            }
        });

    let client = RpcClient::<OuterService, _>::new(client);
    let client: RpcClient<OuterService, _, InnerService> = client.map();
    let client: RpcClient<OuterService, _, ComputeService> = client.map();
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
async fn tokio_mpsc_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = tokio_mpsc::connection::<ComputeService>(1);

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
