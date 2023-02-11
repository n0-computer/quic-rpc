#![allow(clippy::enum_variant_names)]
use async_stream::stream;
use derive_more::{From, TryInto};
use futures::{SinkExt, Stream, StreamExt};
use message::RpcMsg;
use quic_rpc::{
    message::{BidiStreaming, ClientStreaming, Msg, ServerStreaming},
    server::RpcServerError,
    transport::mem,
    Connection, ServerConnection, *,
};
use serde::{Deserialize, Serialize};
use std::{fmt::Debug, result};

type Cid = [u8; 32];
#[derive(Debug, Serialize, Deserialize)]
struct Put(Vec<u8>);
#[derive(Debug, Serialize, Deserialize)]
struct Get(Cid);
#[derive(Debug, Serialize, Deserialize)]
struct PutResponse(Cid);
#[derive(Debug, Serialize, Deserialize)]
struct GetResponse(Vec<u8>);

#[derive(Debug, Serialize, Deserialize)]
struct PutFile;

#[derive(Debug, Serialize, Deserialize)]
struct PutFileUpdate(Vec<u8>);

#[derive(Debug, Serialize, Deserialize)]
struct PutFileResponse(Cid);

#[derive(Debug, Serialize, Deserialize)]
struct GetFile(Cid);

#[derive(Debug, Serialize, Deserialize)]
struct GetFileResponse(Vec<u8>);

#[derive(Debug, Serialize, Deserialize)]
struct ConvertFile;

#[derive(Debug, Serialize, Deserialize)]
struct ConvertFileUpdate(Vec<u8>);

#[derive(Debug, Serialize, Deserialize)]
struct ConvertFileResponse(Vec<u8>);

macro_rules! request_enum {
    // User entry points.
    ($enum_name:ident { $variant_name:ident $($tt:tt)* }) => {
        request_enum!(@ {[$enum_name] [$variant_name]} $($tt)*);
    };

    // Internal rules to categorize each value
    (@ {[$enum_name:ident] [$($agg:ident)*]} $(,)? $variant_name:ident $($tt:tt)*) => {
        request_enum!(@ {[$enum_name] [$($agg)* $variant_name]} $($tt)*);
    };

    // Final internal rule that generates the enum from the categorized input
    (@ {[$enum_name:ident] [$($n:ident)*]} $(,)?) => {
        #[derive(::std::fmt::Debug, ::derive_more::From, ::derive_more::TryInto, ::serde::Serialize, ::serde::Deserialize)]
        enum $enum_name {
            $($n($n),)*
        }
    };
}

request_enum! {
    StoreRequest2 {
        Put,
        Get,
        PutFile, PutFileUpdate,
        GetFile,
        ConvertFile, ConvertFileUpdate,
    }
}

#[derive(Debug, From, TryInto, Serialize, Deserialize)]
enum StoreRequest {
    Put(Put),

    Get(Get),

    PutFile(PutFile),
    PutFileUpdate(PutFileUpdate),

    GetFile(GetFile),

    ConvertFile(ConvertFile),
    ConvertFileUpdate(ConvertFileUpdate),
}

#[derive(Debug, From, TryInto, Serialize, Deserialize)]
enum StoreResponse {
    PutResponse(PutResponse),
    GetResponse(GetResponse),
    PutFileResponse(PutFileResponse),
    GetFileResponse(GetFileResponse),
    ConvertFileResponse(ConvertFileResponse),
}

#[derive(Debug, Clone)]
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

impl Msg<StoreService> for PutFile {
    type Response = PutFileResponse;
    type Update = PutFileUpdate;
    type Pattern = ClientStreaming;
}

impl Msg<StoreService> for GetFile {
    type Response = GetFileResponse;
    type Update = GetFile;
    type Pattern = ServerStreaming;
}

impl Msg<StoreService> for ConvertFile {
    type Response = ConvertFileResponse;
    type Update = ConvertFileUpdate;
    type Pattern = BidiStreaming;
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    async fn server_future<C: ServerConnection<StoreService>>(
        server: RpcServer<StoreService, C>,
    ) -> result::Result<(), RpcServerError<C>> {
        let s = server;
        let store = Store;
        loop {
            let (req, chan) = s.accept_one().await?;
            use StoreRequest::*;
            let store = store.clone();
            #[rustfmt::skip]
            match req {
                Put(msg) => s.rpc(msg, chan, store, Store::put).await,
                Get(msg) => s.rpc(msg, chan, store, Store::get).await,
                PutFile(msg) => s.client_streaming(msg, chan, store, Store::put_file).await,
                GetFile(msg) => s.server_streaming(msg, chan, store, Store::get_file).await,
                ConvertFile(msg) => s.bidi_streaming(msg, chan, store, Store::convert_file).await,
                PutFileUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
                ConvertFileUpdate(_) => Err(RpcServerError::UnexpectedStartMessage)?,
            }?;
        }
    }

    let (server, client) = mem::connection::<StoreRequest, StoreResponse>(1);
    let client = RpcClient::<StoreService, _>::new(client);
    let server = RpcServer::<StoreService, _>::new(server);
    let server_handle = tokio::task::spawn(server_future(server));

    // a rpc call
    println!("a rpc call");
    let res = client.rpc(Get([0u8; 32])).await?;
    println!("{res:?}");

    // server streaming call
    println!("a server streaming call");
    let mut s = client.server_streaming(GetFile([0u8; 32])).await?;
    while let Some(res) = s.next().await {
        println!("{res:?}");
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
    println!("{res:?}");

    // bidi streaming call
    println!("a bidi streaming call");
    let (mut send, mut recv) = client.bidi(ConvertFile).await?;
    tokio::task::spawn(async move {
        for i in 0..3 {
            send.send(ConvertFileUpdate(vec![i])).await.unwrap();
        }
    });
    while let Some(res) = recv.next().await {
        println!("{res:?}");
    }

    // dropping the client will cause the server to terminate
    drop(client);
    server_handle.await??;
    Ok(())
}

async fn _main_unsugared() -> anyhow::Result<()> {
    let (server, client) = mem::connection::<u64, String>(1);
    let to_string_service = tokio::spawn(async move {
        let (mut send, mut recv) = server.next().await?;
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("server got: {item:?}");
            send.send(item.to_string()).await?;
        }
        anyhow::Ok(())
    });
    let (mut send, mut recv) = client.next().await?;
    let print_result_service = tokio::spawn(async move {
        while let Some(item) = recv.next().await {
            let item = item?;
            println!("got result: {item}");
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
