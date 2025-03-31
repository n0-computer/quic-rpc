use std::{collections::BTreeMap, sync::Arc};

use anyhow::bail;
use n0_future::task::{self, AbortOnDropHandle};
use quic_rpc::{
    LocalSender, Request, Service, ServiceSender, WithChannels,
    channel::{oneshot, spsc},
    rpc::Handler,
};
// Import the macro
use quic_rpc_derive::rpc_requests;
use quic_rpc_iroh::{IrohRemoteConnection, listen};
use serde::{Deserialize, Serialize};
use tracing::info;

/// A simple storage service, just to try it out
#[derive(Debug, Clone, Copy)]
struct StorageService;

impl Service for StorageService {}

#[derive(Debug, Serialize, Deserialize)]
struct Get {
    key: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct List;

#[derive(Debug, Serialize, Deserialize)]
struct Set {
    key: String,
    value: String,
}

// Use the macro to generate both the StorageProtocol and StorageMessage enums
// plus implement Channels for each type
#[rpc_requests(StorageService, StorageMessage)]
#[derive(Serialize, Deserialize)]
enum StorageProtocol {
    #[rpc(tx=oneshot::Sender<Option<String>>)]
    Get(Get),
    #[rpc(tx=oneshot::Sender<()>)]
    Set(Set),
    #[rpc(tx=spsc::Sender<String>)]
    List(List),
}

struct StorageActor {
    recv: tokio::sync::mpsc::Receiver<StorageMessage>,
    state: BTreeMap<String, String>,
}

impl StorageActor {
    pub fn local() -> StorageApi {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        let actor = Self {
            recv: rx,
            state: BTreeMap::new(),
        };
        n0_future::task::spawn(actor.run());
        let local = LocalSender::<StorageMessage, StorageService>::from(tx);
        StorageApi {
            inner: local.into(),
        }
    }

    async fn run(mut self) {
        while let Some(msg) = self.recv.recv().await {
            self.handle(msg).await;
        }
    }

    async fn handle(&mut self, msg: StorageMessage) {
        match msg {
            StorageMessage::Get(get) => {
                info!("get {:?}", get);
                let WithChannels { tx, inner, .. } = get;
                tx.send(self.state.get(&inner.key).cloned()).await.ok();
            }
            StorageMessage::Set(set) => {
                info!("set {:?}", set);
                let WithChannels { tx, inner, .. } = set;
                self.state.insert(inner.key, inner.value);
                tx.send(()).await.ok();
            }
            StorageMessage::List(list) => {
                info!("list {:?}", list);
                let WithChannels { mut tx, .. } = list;
                for (key, value) in &self.state {
                    if tx.send(format!("{key}={value}")).await.is_err() {
                        break;
                    }
                }
            }
        }
    }
}

struct StorageApi {
    inner: ServiceSender<StorageMessage, StorageProtocol, StorageService>,
}

impl StorageApi {
    pub fn connect(endpoint: iroh::Endpoint, addr: iroh::NodeAddr) -> anyhow::Result<StorageApi> {
        Ok(StorageApi {
            inner: ServiceSender::boxed(IrohRemoteConnection::new(
                endpoint,
                addr,
                b"RPC-Storage".to_vec(),
            )),
        })
    }

    pub fn listen(&self, endpoint: iroh::Endpoint) -> anyhow::Result<AbortOnDropHandle<()>> {
        let Some(local) = self.inner.local() else {
            bail!("cannot listen on a remote service");
        };
        let local = local.clone();
        let handler: Handler<StorageProtocol> = Arc::new(move |msg, _, tx| {
            let local = local.clone();
            Box::pin(match msg {
                StorageProtocol::Get(msg) => local.send((msg, tx)),
                StorageProtocol::Set(msg) => local.send((msg, tx)),
                StorageProtocol::List(msg) => local.send((msg, tx)),
            })
        });
        Ok(AbortOnDropHandle::new(task::spawn(listen(
            endpoint, handler,
        ))))
    }

    pub async fn get(&self, key: String) -> anyhow::Result<oneshot::Receiver<Option<String>>> {
        let msg = Get { key };
        match self.inner.sender().await? {
            Request::Local(request) => {
                let (tx, rx) = oneshot::channel();
                request.send((msg, tx)).await?;
                Ok(rx)
            }
            Request::Remote(request) => {
                let (_tx, rx) = request.write(msg).await?;
                Ok(rx.into())
            }
        }
    }

    pub async fn list(&self) -> anyhow::Result<spsc::Receiver<String>> {
        let msg = List;
        match self.inner.sender().await? {
            Request::Local(request) => {
                let (tx, rx) = spsc::channel(10);
                request.send((msg, tx)).await?;
                Ok(rx)
            }
            Request::Remote(request) => {
                let (_tx, rx) = request.write(msg).await?;
                Ok(rx.into())
            }
        }
    }

    pub async fn set(&self, key: String, value: String) -> anyhow::Result<oneshot::Receiver<()>> {
        let msg = Set { key, value };
        match self.inner.sender().await? {
            Request::Local(request) => {
                let (tx, rx) = oneshot::channel();
                request.send((msg, tx)).await?;
                Ok(rx)
            }
            Request::Remote(request) => {
                let (_tx, rx) = request.write(msg).await?;
                Ok(rx.into())
            }
        }
    }
}

async fn local() -> anyhow::Result<()> {
    let api = StorageActor::local();
    api.set("hello".to_string(), "world".to_string())
        .await?
        .await?;
    let value = api.get("hello".to_string()).await?.await?;
    let mut list = api.list().await?;
    while let Some(value) = list.recv().await? {
        println!("list value = {:?}", value);
    }
    println!("value = {:?}", value);
    Ok(())
}

async fn remote() -> anyhow::Result<()> {
    let server = iroh::Endpoint::builder()
        .discovery_n0()
        .alpns(vec![b"RPC-Storage".to_vec()])
        .bind()
        .await?;
    let client = iroh::Endpoint::builder().bind().await?;
    let addr = server.node_addr().await?;
    let store = StorageActor::local();
    let handle = store.listen(server)?;
    let api = StorageApi::connect(client, addr)?;
    api.set("hello".to_string(), "world".to_string())
        .await?
        .await?;
    api.set("goodbye".to_string(), "world".to_string())
        .await?
        .await?;
    let value = api.get("hello".to_string()).await?.await?;
    println!("value = {:?}", value);
    let mut list = api.list().await?;
    while let Some(value) = list.recv().await? {
        println!("list value = {:?}", value);
    }
    drop(handle);
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    println!("Local use");
    local().await?;
    println!("Remote use");
    remote().await?;
    Ok(())
}
