use anyhow::Context;
use derive_more::{From, TryInto};
use futures::{Future, Sink, SinkExt, Stream, StreamExt, future::BoxFuture, FutureExt};
use serde::{Serialize, Deserialize};
use std::{fmt::Debug, result};
use sugar::{ClientChannel, RpcMsg};

use crate::sugar::{Service, HandleRpc};
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;
pub mod sugar;

/// An abstract channel to a service
///
/// This assumes cheap streams, so every interaction uses a new stream.
pub trait Channel<Req, Res> {
    /// Sink type
    type ReqSink: Sink<Req, Error = Self::SendError>;
    /// Stream type
    type ResStream: Stream<Item = result::Result<Res, Self::RecvError>>;
    /// Error you might get while sending messages from a stream
    type SendError: Debug;
    /// Error you might get while receiving messages from a stream
    type RecvError: Debug;
    /// Error you might get when opening a new stream
    type OpenBiError: Debug;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::OpenBiError>>
        + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams
    type AcceptBiError: Debug;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<Output = result::Result<(Self::ReqSink, Self::ResStream), Self::AcceptBiError>>
        + 'a
    where
        Self: 'a;
    /// Accept a bidirectional stream
    fn accept_bi(&mut self) -> Self::AcceptBiFuture<'_>;
}

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

#[tokio::main]
async fn main() -> anyhow::Result<()> {

    struct Store;

    #[handlers]
    impl Store {
        async fn handle_put(&self, put: Put) -> PutResponse {
            PutResponse([0; 32])
        }

        async fn handle_get(&self, get: Get) -> GetResponse {
            GetResponse(vec![])
        }

        // makes a dispatcher that takes a stream pair and then does its thing!
    }

    impl HandleRpc<StoreService, Put> for Store {
        type RpcFuture = BoxFuture<'static, PutResponse>;

        fn rpc(&self, _msg: Put) -> Self::RpcFuture {
            async move {
                PutResponse([0u8;32])
            }
            .boxed()
        }
    }

    impl HandleRpc<StoreService, Get> for Store {
        type RpcFuture = BoxFuture<'static, GetResponse>;

        fn rpc(&self, _msg: Get) -> Self::RpcFuture {
            async move {
                GetResponse([0u8;32].to_vec())
            }
            .boxed()
        }
    }

    let store = Store;
    let (client, mut server) = mem::connection::<StoreRequest, StoreResponse>(1);
    let mut client = ClientChannel::<StoreService>::new(client);
    let server_handle = tokio::task::spawn(async move {
        let (send, mut recv) = server.accept_bi().await?;
        let first = recv.next().await.context("no first message")??;
        match first {
            StoreRequest::Put(msg) => {
                store.handle(msg, recv, send).await?;
            }
            StoreRequest::Get(msg) => {
                store.handle(msg, recv, send).await?;
            }
        }
        anyhow::Ok(())
    });
    let res = client.rpc(Get([0u8;32])).await?;
    println!("{:?}", res);
    server_handle.await??;
    Ok(())
}

async fn main_unsugared() -> anyhow::Result<()> {
    let (mut server, mut client) = mem::connection::<String, u64>(1);
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
