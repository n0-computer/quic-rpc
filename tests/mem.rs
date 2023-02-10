mod math;
use math::*;
use quic_rpc::{server::RpcServerError, transport::mem, RpcClient, RpcServer};

#[tokio::test]
async fn mem_channel_bench() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = mem::connection::<ComputeRequest, ComputeResponse>(1);

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

/// simple happy path test for all 4 patterns
#[tokio::test]
async fn mem_channel_smoke() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = mem::connection::<ComputeRequest, ComputeResponse>(1);

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
