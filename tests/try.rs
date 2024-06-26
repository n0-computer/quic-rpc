#![cfg(feature = "flume-transport")]
use derive_more::{From, TryInto};
use futures_lite::{Stream, StreamExt};
use quic_rpc::{
    message::Msg,
    pattern::try_server_streaming::{StreamCreated, TryServerStreaming, TryServerStreamingMsg},
    server::RpcServerError,
    transport::flume,
    RpcClient, RpcServer, Service,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
struct TryService;

impl Service for TryService {
    type Req = TryRequest;
    type Res = TryResponse;
}

#[derive(Debug, Serialize, Deserialize)]
pub struct StreamN {
    n: u64,
}

impl Msg<TryService> for StreamN {
    type Pattern = TryServerStreaming;
}

impl TryServerStreamingMsg<TryService> for StreamN {
    type Item = u64;
    type ItemError = String;
    type CreateError = String;
}

/// request enum
#[derive(Debug, Serialize, Deserialize, From, TryInto)]
pub enum TryRequest {
    StreamN(StreamN),
}

#[derive(Debug, Serialize, Deserialize, From, TryInto, Clone)]
pub enum TryResponse {
    StreamN(std::result::Result<u64, String>),
    StreamNError(std::result::Result<StreamCreated, String>),
}

#[derive(Clone)]
struct Handler;

impl Handler {
    async fn try_stream_n(
        self,
        req: StreamN,
    ) -> std::result::Result<impl Stream<Item = std::result::Result<u64, String>>, String> {
        if req.n % 2 != 0 {
            return Err("odd n not allowed".to_string());
        }
        let stream = async_stream::stream! {
            for i in 0..req.n {
                if i > 5 {
                    yield Err("n too large".to_string());
                    return;
                }
                yield Ok(i);
            }
        };
        Ok(stream)
    }
}

#[tokio::test]
async fn try_server_streaming() -> anyhow::Result<()> {
    tracing_subscriber::fmt::try_init().ok();
    let (server, client) = flume::connection::<TryService>(1);

    let server = RpcServer::<TryService, _>::new(server);
    let server_handle = tokio::task::spawn(async move {
        loop {
            let (req, chan) = server.accept_and_read_first().await?;
            let handler = Handler;
            match req {
                TryRequest::StreamN(req) => {
                    chan.try_server_streaming(req, handler, Handler::try_stream_n)
                        .await?;
                }
            }
        }
        #[allow(unreachable_code)]
        Ok(())
    });
    let client = RpcClient::<TryService, _>::new(client);
    let stream_n = client.try_server_streaming(StreamN { n: 10 }).await?;
    let items: Vec<_> = stream_n.collect().await;
    println!("{:?}", items);
    drop(client);
    // dropping the client will cause the server to terminate
    match server_handle.await? {
        Err(RpcServerError::Accept(_)) => {}
        e => panic!("unexpected termination result {e:?}"),
    }
    Ok(())
}
