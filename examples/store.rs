use anyhow::Context;
use derive_more::{From, TryInto};
use futures::{future::BoxFuture, Future, FutureExt, SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    fmt::{Debug, Display},
    result,
};
use sugar::{ClientChannel, RpcMsg};

use crate::sugar::HandleRpc;
use quic_rpc::{sugar::Msg, Channel, *};

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

pub struct DispatchHelper<S, C> {
    _s: std::marker::PhantomData<(S, C)>,
}

impl<S, C> Clone for DispatchHelper<S, C> {
    fn clone(&self) -> Self {
        Self {
            _s: std::marker::PhantomData,
        }
    }
}

impl<S, C> Copy for DispatchHelper<S, C> {}

pub enum HandleOneError<S: Service, C: quic_rpc::Channel<S::Req, S::Res>> {
    AcceptBiError(C::AcceptBiError),
    EarlyClose,
    RecvError(C::RecvError),
    SendError(C::SendError),
}

impl<S: Service, C: quic_rpc::Channel<S::Req, S::Res>> Debug for HandleOneError<S, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AcceptBiError(arg0) => f.debug_tuple("AcceptBiError").field(arg0).finish(),
            Self::EarlyClose => write!(f, "EarlyClose"),
            Self::RecvError(arg0) => f.debug_tuple("RecvError").field(arg0).finish(),
            Self::SendError(arg0) => f.debug_tuple("SendError").field(arg0).finish(),
        }
    }
}

impl<S: Service, C: quic_rpc::Channel<S::Req, S::Res>> Display for HandleOneError<S, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Debug::fmt(&self, f)
    }
}

impl<S: Service, C: quic_rpc::Channel<S::Req, S::Res>> std::error::Error for HandleOneError<S, C> {}

impl<S: Service, C: quic_rpc::Channel<S::Req, S::Res>> DispatchHelper<S, C> {
    fn new() -> Self {
        Self {
            _s: std::marker::PhantomData,
        }
    }

    /// handle the message M using the given function on the target object
    ///
    /// If you want to support concurrent requests, you need to spawn this on a tokio task yourself.
    pub async fn rpc<M, F, Fut, T>(
        self,
        req: M,
        c: (C::SendSink<S::Res>, C::RecvStream<S::Req>),
        target: T,
        f: F,
    ) -> result::Result<(), C::SendError>
    where
        M: Msg<S>,
        F: FnOnce(T, M) -> Fut,
        Fut: Future<Output = M::Response>,
    {
        let (send, _recv) = c;
        // get the response
        let res = f(target, req).await;
        // turn into a S::Res so we can send it
        let res: S::Res = res.into();
        // send it and return the error if any
        tokio::pin!(send);
        send.send(res).await
    }

    pub async fn accept_one(
        self,
        channel: &mut C,
    ) -> result::Result<(S::Req, (C::SendSink<S::Res>, C::RecvStream<S::Req>)), HandleOneError<S, C>>
    where
        C::RecvStream<S::Req>: Unpin,
    {
        let mut channel = channel
            .accept_bi()
            .await
            .map_err(HandleOneError::AcceptBiError)?;
        // get the first message from the client. This will tell us what it wants to do.
        let request: S::Req = channel
            .1
            .next()
            .await
            // no msg => early close
            .ok_or(HandleOneError::EarlyClose)?
            // recv error
            .map_err(HandleOneError::RecvError)?;
        Ok((request, channel))
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    #[derive(Clone)]
    struct Store;
    impl Store {
        async fn put(self, put: Put) -> PutResponse {
            PutResponse([0; 32])
        }

        async fn get(self, get: Get) -> GetResponse {
            GetResponse(vec![])
        }

        // makes a dispatcher that takes a stream pair and then does its thing!
    }

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
    let (client, mut server) = mem::connection::<StoreResponse, StoreRequest>(1);
    let mut client = ClientChannel::<StoreService>::new(client);
    let server_handle = tokio::task::spawn(async move {
        let d = DispatchHelper::new();
        loop {
            let (req, chan) = d.accept_one(&mut server).await?;
            use StoreRequest::*;
            let store = store.clone();
            match req {
                Put(msg) => d.rpc(msg, chan, store, Store::put).await,
                Get(msg) => d.rpc(msg, chan, store, Store::get).await,
            }
            .map_err(HandleOneError::SendError)?;
        }
        Ok::<(), HandleOneError<StoreService, _>>(())
    });
    let res = client.rpc(Get([0u8; 32])).await?;
    println!("{:?}", res);
    drop(client);
    server_handle.await??;
    Ok(())
}

async fn main_unsugared() -> anyhow::Result<()> {
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
