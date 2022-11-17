use anyhow::Context;
use derive_more::{From, TryInto};
use futures::{future::BoxFuture, Future, FutureExt, Sink, SinkExt, Stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::{fmt::Debug, result};
use sugar::{ClientChannel, RpcMsg};

use crate::sugar::{HandleRpc, Service};
pub mod mem;
pub mod mem_and_quinn;
pub mod mem_or_quinn;
pub mod quinn;
pub mod sugar;

/// An abstract channel to a service
///
/// This assumes cheap streams, so every interaction uses a new stream.
pub trait Channel<Req: Serialize + DeserializeOwned, Res: Serialize + DeserializeOwned> {
    /// The sink used for sending either requests or responses on this channel
    type SendSink<M: Serialize>: Sink<M, Error = Self::SendError>;
    /// The stream used for receiving either requests or responses on this channel
    type RecvStream<M: DeserializeOwned>: Stream<Item = result::Result<M, Self::RecvError>>;
    /// Error you might get while sending messages to a sink
    type SendError: Debug;
    /// Error you might get while receiving messages from a stream
    type RecvError: Debug;
    /// Error you might get when opening a new connection to the server
    type OpenBiError: Debug;
    /// Future returned by open_bi
    type OpenBiFuture<'a>: Future<
            Output = result::Result<
                (Self::SendSink<Req>, Self::RecvStream<Res>),
                Self::OpenBiError,
            >,
        > + 'a
    where
        Self: 'a;
    /// Open a bidirectional stream
    fn open_bi(&mut self) -> Self::OpenBiFuture<'_>;
    /// Error you might get when waiting for new streams on the server side
    type AcceptBiError: Debug;
    /// Future returned by accept_bi
    type AcceptBiFuture<'a>: Future<
            Output = result::Result<
                (Self::SendSink<Req>, Self::RecvStream<Res>),
                Self::AcceptBiError,
            >,
        > + 'a
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

// struct DispatchHelper<This, S, C> {
//     this: This,
//     _sc: std::marker::PhantomData<(S, C)>,
// }

// impl<This, S: Service, C: crate::Channel<S::Req, S::Res>> DispatchHelper<This, S, C> {

//     /// handle the message M using the given function on the target object
//     pub async fn handle_rpc<M, F, Fut>(this: &This, req: M, mut c: (mem::ReqSink<S::Res>, mem::ResStream<S::Req>), f: F) -> result::Result<(), C::SendError>
//         where
//             M: Msg<S>,
//             F: FnOnce(&This, M) -> Fut,
//             Fut: Future<Output = M::Response>
//     {
//         // get the response
//         let res = f(this, req).await;
//         // turn into a S::Res so we can send it
//         let res: S::Res = res.into();
//         // send it and return the error if any
//         c.0.send(res).await
//     }
// }

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    struct Store;

    // #[handlers]
    // impl Store {
    //     async fn handle_put(&self, put: Put) -> PutResponse {
    //         PutResponse([0; 32])
    //     }

    //     async fn handle_get(&self, get: Get) -> GetResponse {
    //         GetResponse(vec![])
    //     }

    //     // makes a dispatcher that takes a stream pair and then does its thing!
    // }

    impl HandleRpc<StoreService, Put> for Store {
        type RpcFuture = BoxFuture<'static, PutResponse>;

        fn rpc(&self, _msg: Put) -> Self::RpcFuture {
            async move { PutResponse([0u8; 32]) }.boxed()
        }
    }

    impl HandleRpc<StoreService, Get> for Store {
        type RpcFuture = BoxFuture<'static, GetResponse>;

        fn rpc(&self, _msg: Get) -> Self::RpcFuture {
            async move { GetResponse([0u8; 32].to_vec()) }.boxed()
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
    let res = client.rpc(Get([0u8; 32])).await?;
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
