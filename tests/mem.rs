mod math;

use futures::{SinkExt, StreamExt};
use math::*;
use quic_rpc::{
    mem::{self, MemChannelTypes},
    sugar::{ClientChannel, RpcServerError, ServerChannel},
};
use std::io::{self, Write};

#[tokio::test]
async fn mem_channel_bench() -> anyhow::Result<()> {
    type C = MemChannelTypes;
    let (client, server) = mem::connection::<ComputeResponse, ComputeRequest>(1);

    let server = ServerChannel::<ComputeService, MemChannelTypes>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    let mut client = ClientChannel::<ComputeService, C>::new(client);
    let n = 1000000;
    // individual RPCs
    let mut sum = 0;
    let t0 = std::time::Instant::now();
    for i in 0..1000000 {
        sum += client.rpc(Sqr(i)).await?.0;
        if i % 10000 == 0 {
            print!(".");
            io::stdout().flush()?;
        }
    }
    println!(
        "\nRPC {} {} rps",
        sum,
        (n as f64) / t0.elapsed().as_secs_f64()
    );
    let t0 = std::time::Instant::now();
    let (send, recv) = client.bidi(Multiply(2)).await?;
    let handle = tokio::task::spawn(async move {
        let mut send = send;
        for i in 0..n {
            send.send(MultiplyUpdate(i)).await?;
        }
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
    println!(
        "\nbidi {} {} rps",
        sum,
        (n as f64) / t0.elapsed().as_secs_f64()
    );
    handle.await??;
    drop(client);
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
    let (client, server) = mem::connection::<ComputeResponse, ComputeRequest>(1);

    let server = ServerChannel::<ComputeService, MemChannelTypes>::new(server);
    let server_handle = tokio::task::spawn(ComputeService::server(server));
    smoke_test::<MemChannelTypes>(client).await?;

    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::AcceptBiError(_)) => {}
        e => panic!("unexpected termination result {:?}", e),
    }
    Ok(())
}
