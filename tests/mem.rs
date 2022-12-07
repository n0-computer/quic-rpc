mod math;
use math::*;
use quic_rpc::{
    mem::{self, MemChannelTypes},
    server::RpcServerError,
    RpcClient, RpcServer,
};

#[tokio::test]
async fn mem_channel_bench() -> anyhow::Result<()> {
    type C = MemChannelTypes;
    let (server, client) = mem::connection::<ComputeRequest, ComputeResponse>(1);

    let server = RpcServer::<ComputeService, MemChannelTypes>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    let client = RpcClient::<ComputeService, C>::new(client);
    bench(client, 1000000).await?;
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::AcceptBiError(_)) => {}
        e => panic!("unexpected termination result {:?}", e),
    }
    Ok(())
}

/// simple happy path test for all 4 patterns
#[tokio::test]
async fn mem_channel_smoke() -> anyhow::Result<()> {
    let (server, client) = mem::connection::<ComputeRequest, ComputeResponse>(1);

    let server = RpcServer::<ComputeService, MemChannelTypes>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    smoke_test::<MemChannelTypes>(client).await?;

    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::AcceptBiError(_)) => {}
        e => panic!("unexpected termination result {:?}", e),
    }
    Ok(())
}
