use derive_more::{Display, From, TryInto};
use quic_rpc::{message::RpcMsg, RpcClient, RpcServer, Service};
use serde::{Deserialize, Serialize};
use std::result;

#[derive(Debug, Serialize, Deserialize)]
struct WriteRequest(String, Vec<u8>);

#[derive(Debug, Serialize, Deserialize, From, TryInto)]
enum IoRequest {
    Write(WriteRequest),
}

/// Serializable wire error type. There has to be a From instance from the convenience error type.
///
/// The RPC client sees this type directly.
#[derive(Debug, Display, Serialize, Deserialize)]
struct WriteError(String);

impl std::error::Error for WriteError {}

impl From<anyhow::Error> for WriteError {
    fn from(e: anyhow::Error) -> Self {
        WriteError(format!("{e:?}"))
    }
}

#[derive(Debug, Serialize, Deserialize, From, TryInto)]
enum IoResponse {
    Write(result::Result<(), WriteError>),
}

#[derive(Debug, Clone)]
struct IoService;

impl Service for IoService {
    type Req = IoRequest;
    type Res = IoResponse;
}

impl RpcMsg<IoService> for WriteRequest {
    type Response = result::Result<(), WriteError>;
}

#[derive(Debug, Clone, Copy)]
struct Fs;

impl Fs {
    /// write a file, returning the convenient anyhow::Result
    async fn write(self, _req: WriteRequest) -> anyhow::Result<()> {
        Ok(())
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let fs = Fs;
    let (server, client) = quic_rpc::transport::flume::connection(1);
    let client = RpcClient::<IoService, _>::new(client);
    let server = RpcServer::<IoService, _>::new(server);
    let handle = tokio::task::spawn(async move {
        for _ in 0..1 {
            let (req, chan) = server.accept().await?;
            match req {
                IoRequest::Write(req) => chan.rpc_map_err(req, fs, Fs::write).await,
            }?
        }
        anyhow::Ok(())
    });
    client
        .rpc(WriteRequest("hello".to_string(), vec![0u8; 32]))
        .await??;
    handle.await??;
    Ok(())
}
