use anyhow::Context;
use quic_rpc::{server::RpcServerError, transport::Connection};

#[allow(unused)]
pub async fn check_termination_anyhow<C: Connection>(
    server_handle: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(e) => {
            let err: RpcServerError<C> = e.downcast().context("unexpected termination result")?;
            match err {
                RpcServerError::Accept(_) => {}
                e => panic!("unexpected termination error {e:?}"),
            }
        }
        e => panic!("server should have terminated with an error {e:?}"),
    }
    Ok(())
}
