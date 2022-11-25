use std::{fmt::Debug, sync::Arc};

use quic_rpc::{
    channel_factory::LazyChannelFactory,
    mem::{self, MemChannelTypes},
    rpc_service, RpcClient, RpcServer,
};
use serde::{Deserialize, Serialize};

// define a tiny test service
#[derive(Debug, Serialize, Deserialize)]
pub struct PingRequest;

#[derive(Debug, Serialize, Deserialize)]
pub struct PingResponse;

rpc_service! {
    Request = TestRequest;
    Response = TestResponse;
    Service = TestService;
    CreateDispatch = _;
    CreateClient = _;

    Rpc ping = PingRequest, _ -> PingResponse;
}

struct PingServer;

impl PingServer {
    async fn ping(self, _req: PingRequest) -> PingResponse {
        PingResponse
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    type S = TestService;
    type C = MemChannelTypes;
    let (c, s) = mem::connection(1);
    // start the test service
    let server_handle = tokio::task::spawn(async move {
        let mut s = RpcServer::<S, C>::new(s);
        loop {
            let (req, chan) = s.accept_one().await?;
            match req {
                TestRequest::PingRequest(req) => {
                    s.rpc(req, chan, PingServer, PingServer::ping).await
                }
            }?
        }
        #[allow(unreachable_code)]
        anyhow::Ok(())
    });
    let cf = LazyChannelFactory::<_, _, C, _>::super_lazy(move || {
        let c = c.clone();
        async move { Ok(c) }
    });
    let mut client: RpcClient<S, C> = quic_rpc::client::RpcClient::from_factory(Arc::new(cf));
    let _res = client.rpc(PingRequest).await?;
    server_handle.abort();
    server_handle.await??;
    Ok(())
}
