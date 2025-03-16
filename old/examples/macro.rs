mod store_rpc {
    use std::fmt::Debug;

    use quic_rpc::rpc_service;
    use serde::{Deserialize, Serialize};

    pub type Cid = [u8; 32];

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Put(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutResponse(pub Cid);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct Get(pub Cid);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetResponse(pub Vec<u8>);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFile;
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFileUpdate(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct PutFileResponse(pub Cid);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetFile(pub Cid);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct GetFileResponse(pub Vec<u8>);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFile;
    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFileUpdate(pub Vec<u8>);
    #[derive(Debug, Serialize, Deserialize)]
    pub struct ConvertFileResponse(pub Vec<u8>);

    rpc_service! {
        Request = StoreRequest;
        Response = StoreResponse;
        Service = StoreService;
        CreateDispatch = create_store_dispatch;

        Rpc put = Put, _ -> PutResponse;
        Rpc get = Get, _ -> GetResponse;
        ClientStreaming put_file = PutFile, PutFileUpdate -> PutFileResponse;
        ServerStreaming get_file = GetFile, _ -> GetFileResponse;
        BidiStreaming convert_file = ConvertFile, ConvertFileUpdate -> ConvertFileResponse;
    }
}

use async_stream::stream;
use futures_lite::{Stream, StreamExt};
use futures_util::SinkExt;
use quic_rpc::{client::RpcClient, server::run_server_loop, transport::flume};
use store_rpc::*;

#[derive(Clone)]
pub struct Store;

impl Store {
    async fn put(self, _put: Put) -> PutResponse {
        PutResponse([0; 32])
    }

    async fn get(self, _get: Get) -> GetResponse {
        GetResponse(vec![])
    }

    async fn put_file(
        self,
        _put: PutFile,
        updates: impl Stream<Item = PutFileUpdate>,
    ) -> PutFileResponse {
        tokio::pin!(updates);
        while let Some(_update) = updates.next().await {}
        PutFileResponse([0; 32])
    }

    fn get_file(self, _get: GetFile) -> impl Stream<Item = GetFileResponse> + Send + 'static {
        stream! {
            for i in 0..3 {
                yield GetFileResponse(vec![i]);
            }
        }
    }

    fn convert_file(
        self,
        _convert: ConvertFile,
        updates: impl Stream<Item = ConvertFileUpdate> + Send + 'static,
    ) -> impl Stream<Item = ConvertFileResponse> + Send + 'static {
        stream! {
            tokio::pin!(updates);
            while let Some(msg) = updates.next().await {
                yield ConvertFileResponse(msg.0);
            }
        }
    }
}

create_store_dispatch!(Store, dispatch_store_request);
// create_store_client!(StoreClient);

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (server, client) = flume::channel(1);
    let server_handle = tokio::task::spawn(async move {
        let target = Store;
        run_server_loop(StoreService, server, target, dispatch_store_request).await
    });
    let client = RpcClient::<StoreService, _>::new(client);

    // a rpc call
    for i in 0..3 {
        println!("a rpc call [{i}]");
        let client = client.clone();
        tokio::task::spawn(async move {
            let res = client.rpc(Get([0u8; 32])).await;
            println!("rpc res [{i}]: {res:?}");
        });
    }

    // server streaming call
    println!("a server streaming call");
    let mut s = client.server_streaming(GetFile([0u8; 32])).await?;
    while let Some(res) = s.next().await {
        println!("streaming res: {res:?}");
    }

    // client streaming call
    println!("a client streaming call");
    let (mut send, recv) = client.client_streaming(PutFile).await?;
    tokio::task::spawn(async move {
        for i in 0..3 {
            send.send(PutFileUpdate(vec![i])).await.unwrap();
        }
    });
    let res = recv.await?;
    println!("client stremaing res: {res:?}");

    // bidi streaming call
    println!("a bidi streaming call");
    let (mut send, mut recv) = client.bidi(ConvertFile).await?;
    tokio::task::spawn(async move {
        for i in 0..3 {
            send.send(ConvertFileUpdate(vec![i])).await.unwrap();
        }
    });
    while let Some(res) = recv.next().await {
        println!("bidi res: {res:?}");
    }

    // dropping the client will cause the server to terminate
    drop(client);
    server_handle.await??;
    Ok(())
}
