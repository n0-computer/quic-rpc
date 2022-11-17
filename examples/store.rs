use derive_more::{From, TryInto};
use futures::{SinkExt, StreamExt};
use quic_rpc::{
    sugar::{DispatchHelper, HandleOneError},
    Channel, *,
};
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, result};
use sugar::{ClientChannel, RpcMsg};

type Cid = [u8; 32];
#[derive(Debug, Serialize, Deserialize)]
struct Put(Vec<u8>);
#[derive(Debug, Serialize, Deserialize)]
struct Get(Cid);
#[derive(Debug, Serialize, Deserialize)]
struct PutResponse(Cid);
#[derive(Debug, Serialize, Deserialize)]
struct GetResponse(Vec<u8>);

#[derive(Debug, From, TryInto, Serialize, Deserialize)]
enum StoreRequest {
    Put(Put),
    Get(Get),
}

#[derive(Debug, From, TryInto, Serialize, Deserialize)]
enum StoreResponse {
    PutResponse(PutResponse),
    GetResponse(GetResponse),
}

struct StoreService;
impl Service for StoreService {
    type Req = StoreRequest;
    type Res = StoreResponse;
}

impl RpcMsg<StoreService> for Put {
    type Response = PutResponse;
}

impl RpcMsg<StoreService> for Get {
    type Response = GetResponse;
}

#[derive(Clone)]
struct Store;
impl Store {
    async fn put(self, _put: Put) -> PutResponse {
        PutResponse([0; 32])
    }

    async fn get(self, _get: Get) -> GetResponse {
        GetResponse(vec![])
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    type Chan = mem::Channel<StoreRequest, StoreResponse>;
    async fn server_future(
        mut server: Chan,
    ) -> result::Result<(), HandleOneError<StoreService, Chan>> {
        let store = Store;
        let d = DispatchHelper::default();
        loop {
            let (req, chan) = d.accept_one(&mut server).await?;
            use StoreRequest::*;
            let store = store.clone();
            let res = match req {
                Put(msg) => d.rpc(msg, chan, store, Store::put).await,
                Get(msg) => d.rpc(msg, chan, store, Store::get).await,
            };
            res.map_err(HandleOneError::SendError)?;
        }
    }

    let (client, server) = mem::connection::<StoreResponse, StoreRequest>(1);
    let mut client = ClientChannel::<StoreService>::new(client);
    let server_handle = tokio::task::spawn(server_future(server));
    let res = client.rpc(Get([0u8; 32])).await?;
    println!("{:?}", res);
    drop(client);
    server_handle.await??;
    Ok(())
}

async fn _main_unsugared() -> anyhow::Result<()> {
    let (mut server, mut client) = mem::connection::<u64, String>(1);
    let to_string_service = tokio::spawn(async move {
        let (mut send, mut recv) = server.accept_bi().await?;
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("server got: {:?}", item);
            send.send(item.to_string()).await?;
        }
        anyhow::Ok(())
    });
    let (mut send, mut recv) = client.open_bi().await?;
    let print_result_service = tokio::spawn(async move {
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("got result: {}", item);
        }
        anyhow::Ok(())
    });
    for i in 0..100 {
        send.send(i).await?;
    }
    drop(send);
    to_string_service.await??;
    print_result_service.await??;
    Ok(())
}
