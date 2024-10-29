//! This example shows how an RPC service can be modularized, even between different crates.
//!
//! * `app` module is the top level. it composes `iroh` plus one handler of the app itself
//! * `iroh` module composes two other services, `calc` and `clock`
//!
//! The [`calc`] and [`clock`] modules both expose a [`quic_rpc::Service`] in a regular fashion.
//! They do not `use` anything from `super` or `app` so they could live in their own crates
//! unchanged.

use anyhow::Result;
use futures_lite::StreamExt;
use futures_util::SinkExt;
use quic_rpc::{transport::flume, RpcClient, RpcServer, ServiceConnection, ServiceEndpoint};
use tracing::warn;

use app::AppService;

#[tokio::main]
async fn main() -> Result<()> {
    // Spawn an inmemory connection.
    // Could use quic equally (all code in this example is generic over the transport)
    let (server_conn, client_conn) = flume::service_connection::<AppService>(1);

    // spawn the server
    let handler = app::Handler::default();
    tokio::task::spawn(run_server(server_conn, handler));

    // run a client demo
    client_demo(client_conn).await?;

    Ok(())
}

async fn run_server<C: ServiceEndpoint<AppService>>(server_conn: C, handler: app::Handler) {
    let server = RpcServer::<AppService, _>::new(server_conn);
    loop {
        let Ok(accepting) = server.accept().await else {
            continue;
        };
        match accepting.read_first().await {
            Err(err) => warn!(?err, "server accept failed"),
            Ok((req, chan)) => {
                let handler = handler.clone();
                tokio::task::spawn(async move {
                    if let Err(err) = handler.handle_rpc_request(req, chan).await {
                        warn!(?err, "internal rpc error");
                    }
                });
            }
        }
    }
}
pub async fn client_demo<C: ServiceConnection<AppService>>(conn: C) -> Result<()> {
    let rpc_client = RpcClient::new(conn);
    let client = app::Client::new(rpc_client.clone());

    // call a method from the top-level app client
    let res = client.app_version().await?;
    println!("app_version: {res:?}");

    // call a method from the wrapped iroh.calc client
    let res = client.iroh.calc.add(40, 2).await?;
    println!("iroh.calc.add: {res:?}");

    // can also do "raw" calls without using the wrapped clients
    let res = rpc_client
        .clone()
        .map::<iroh::IrohService>()
        .map::<calc::CalcService>()
        .rpc(calc::AddRequest(19, 4))
        .await?;
    println!("iroh.calc.add (raw): {res:?}");

    let (mut sink, res) = rpc_client
        .map::<iroh::IrohService>()
        .map::<calc::CalcService>()
        .client_streaming(calc::SumRequest)
        .await?;
    sink.send(calc::SumUpdate(4)).await.unwrap();
    sink.send(calc::SumUpdate(8)).await.unwrap();
    sink.send(calc::SumUpdate(30)).await.unwrap();
    drop(sink);
    let res = res.await?;
    println!("iroh.calc.sum (raw): {res:?}");

    // call a server-streaming method from the wrapped iroh.clock client
    let mut stream = client.iroh.clock.tick().await?;
    while let Some(tick) = stream.try_next().await? {
        println!("iroh.clock.tick: {tick}");
    }
    Ok(())
}

mod app {
    //! This is the app-specific code.
    //!
    //! It composes all of `iroh` (which internally composes two other modules) and adds an
    //! application specific RPC.
    //!
    //! It could also easily compose services from other crates or internal modules.

    use anyhow::Result;
    use derive_more::{From, TryInto};
    use quic_rpc::{
        message::RpcMsg, server::RpcChannel, RpcClient, Service, ServiceConnection, ServiceEndpoint,
    };
    use serde::{Deserialize, Serialize};

    use super::iroh;

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Request {
        Iroh(iroh::Request),
        AppVersion(AppVersionRequest),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Response {
        Iroh(iroh::Response),
        AppVersion(AppVersionResponse),
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AppVersionRequest;

    impl RpcMsg<AppService> for AppVersionRequest {
        type Response = AppVersionResponse;
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AppVersionResponse(pub String);

    #[derive(Copy, Clone, Debug)]
    pub struct AppService;
    impl Service for AppService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone)]
    pub struct Handler {
        iroh: iroh::Handler,
        app_version: String,
    }

    impl Default for Handler {
        fn default() -> Self {
            Self {
                iroh: iroh::Handler::default(),
                app_version: "v0.1-alpha".to_string(),
            }
        }
    }

    impl Handler {
        pub async fn handle_rpc_request<E: ServiceEndpoint<AppService>>(
            self,
            req: Request,
            chan: RpcChannel<AppService, AppService, E>,
        ) -> Result<()> {
            match req {
                Request::Iroh(req) => self.iroh.handle_rpc_request(req, chan.map()).await?,
                Request::AppVersion(req) => chan.rpc(req, self, Self::on_version).await?,
            };
            Ok(())
        }

        pub async fn on_version(self, _req: AppVersionRequest) -> AppVersionResponse {
            AppVersionResponse(self.app_version.clone())
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<S: Service, C: ServiceConnection<S>> {
        pub iroh: iroh::Client<S, C>,
        client: RpcClient<AppService, S, C>,
    }

    impl<S, C> Client<S, C>
    where
        S: Service,
        C: ServiceConnection<S>,
    {
        pub fn new(client: RpcClient<AppService, S, C>) -> Self {
            Self {
                iroh: iroh::Client::new(client.clone().map()),
                client,
            }
        }

        pub async fn app_version(&self) -> Result<String> {
            let res = self.client.rpc(AppVersionRequest).await?;
            Ok(res.0)
        }
    }
}

mod iroh {
    //! This module composes two sub-services. Think `iroh` crate which exposes services and
    //! clients for iroh-bytes and iroh-gossip or so.
    //! It uses only the `calc` and `clock` modules and nothing else.

    use anyhow::Result;
    use derive_more::{From, TryInto};
    use quic_rpc::{server::RpcChannel, RpcClient, Service, ServiceConnection, ServiceEndpoint};
    use serde::{Deserialize, Serialize};

    use super::{calc, clock};

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Request {
        Calc(calc::Request),
        Clock(clock::Request),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Response {
        Calc(calc::Response),
        Clock(clock::Response),
    }

    #[derive(Copy, Clone, Debug)]
    pub struct IrohService;
    impl Service for IrohService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone, Default)]
    pub struct Handler {
        calc: calc::Handler,
        clock: clock::Handler,
    }

    impl Handler {
        pub async fn handle_rpc_request<S, E>(
            self,
            req: Request,
            chan: RpcChannel<IrohService, S, E>,
        ) -> Result<()>
        where
            S: Service,
            E: ServiceEndpoint<S>,
        {
            match req {
                Request::Calc(req) => self.calc.handle_rpc_request(req, chan.map()).await?,
                Request::Clock(req) => self.clock.handle_rpc_request(req, chan.map()).await?,
            }
            Ok(())
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<S, C> {
        pub calc: calc::Client<S, C>,
        pub clock: clock::Client<S, C>,
    }

    impl<S, C> Client<S, C>
    where
        S: Service,
        C: ServiceConnection<S>,
    {
        pub fn new(client: RpcClient<IrohService, S, C>) -> Self {
            Self {
                calc: calc::Client::new(client.clone().map()),
                clock: clock::Client::new(client.clone().map()),
            }
        }
    }
}

mod calc {
    //! This is a library providing a service, and a client. E.g. iroh-bytes or iroh-hypermerge.
    //! It does not use any `super` imports, it is completely decoupled.

    use anyhow::{bail, Result};
    use derive_more::{From, TryInto};
    use futures_lite::{Stream, StreamExt};
    use quic_rpc::{
        message::{ClientStreaming, ClientStreamingMsg, Msg, RpcMsg},
        server::RpcChannel,
        RpcClient, Service, ServiceConnection, ServiceEndpoint,
    };
    use serde::{Deserialize, Serialize};
    use std::fmt::Debug;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AddRequest(pub i64, pub i64);

    impl RpcMsg<CalcService> for AddRequest {
        type Response = AddResponse;
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct AddResponse(pub i64);

    #[derive(Debug, Serialize, Deserialize)]
    pub struct SumRequest;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct SumUpdate(pub i64);

    impl Msg<CalcService> for SumRequest {
        type Pattern = ClientStreaming;
    }

    impl ClientStreamingMsg<CalcService> for SumRequest {
        type Update = SumUpdate;
        type Response = SumResponse;
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct SumResponse(pub i64);

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Request {
        Add(AddRequest),
        Sum(SumRequest),
        SumUpdate(SumUpdate),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Response {
        Add(AddResponse),
        Sum(SumResponse),
    }

    #[derive(Copy, Clone, Debug)]
    pub struct CalcService;
    impl Service for CalcService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone, Default)]
    pub struct Handler;

    impl Handler {
        pub async fn handle_rpc_request<S, E>(
            self,
            req: Request,
            chan: RpcChannel<CalcService, S, E>,
        ) -> Result<()>
        where
            S: Service,
            E: ServiceEndpoint<S>,
        {
            match req {
                Request::Add(req) => chan.rpc(req, self, Self::on_add).await?,
                Request::Sum(req) => chan.client_streaming(req, self, Self::on_sum).await?,
                Request::SumUpdate(_) => bail!("Unexpected update message at start of request"),
            }
            Ok(())
        }

        pub async fn on_add(self, req: AddRequest) -> AddResponse {
            AddResponse(req.0 + req.1)
        }

        pub async fn on_sum(
            self,
            _req: SumRequest,
            updates: impl Stream<Item = SumUpdate>,
        ) -> SumResponse {
            let mut sum = 0i64;
            tokio::pin!(updates);
            while let Some(SumUpdate(n)) = updates.next().await {
                sum += n;
            }
            SumResponse(sum)
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<S, C> {
        client: RpcClient<CalcService, S, C>,
    }

    impl<S, C> Client<S, C>
    where
        C: ServiceConnection<S>,
        S: Service,
    {
        pub fn new(client: RpcClient<CalcService, S, C>) -> Self {
            Self { client }
        }
        pub async fn add(&self, a: i64, b: i64) -> anyhow::Result<i64> {
            let res = self.client.rpc(AddRequest(a, b)).await?;
            Ok(res.0)
        }
    }
}

mod clock {
    //! This is a library providing a service, and a client. E.g. iroh-bytes or iroh-hypermerge.
    //! It does not use any `super` imports, it is completely decoupled.

    use anyhow::Result;
    use derive_more::{From, TryInto};
    use futures_lite::{stream::Boxed as BoxStream, Stream, StreamExt};
    use futures_util::TryStreamExt;
    use quic_rpc::{
        message::{Msg, ServerStreaming, ServerStreamingMsg},
        server::RpcChannel,
        RpcClient, Service, ServiceConnection, ServiceEndpoint,
    };
    use serde::{Deserialize, Serialize};
    use std::{
        fmt::Debug,
        sync::{Arc, RwLock},
        time::Duration,
    };
    use tokio::sync::Notify;

    #[derive(Debug, Serialize, Deserialize)]
    pub struct TickRequest;

    impl Msg<ClockService> for TickRequest {
        type Pattern = ServerStreaming;
    }

    impl ServerStreamingMsg<ClockService> for TickRequest {
        type Response = TickResponse;
    }

    #[derive(Debug, Serialize, Deserialize)]
    pub struct TickResponse {
        tick: usize,
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Request {
        Tick(TickRequest),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Response {
        Tick(TickResponse),
    }

    #[derive(Copy, Clone, Debug)]
    pub struct ClockService;
    impl Service for ClockService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone)]
    pub struct Handler {
        tick: Arc<RwLock<usize>>,
        ontick: Arc<Notify>,
    }

    impl Default for Handler {
        fn default() -> Self {
            Self::new(Duration::from_secs(1))
        }
    }

    impl Handler {
        pub fn new(tick_duration: Duration) -> Self {
            let h = Handler {
                tick: Default::default(),
                ontick: Default::default(),
            };
            let h2 = h.clone();
            tokio::task::spawn(async move {
                loop {
                    tokio::time::sleep(tick_duration).await;
                    *h2.tick.write().unwrap() += 1;
                    h2.ontick.notify_waiters();
                }
            });
            h
        }

        pub async fn handle_rpc_request<S, E>(
            self,
            req: Request,
            chan: RpcChannel<ClockService, S, E>,
        ) -> Result<()>
        where
            S: Service,
            E: ServiceEndpoint<S>,
        {
            match req {
                Request::Tick(req) => chan.server_streaming(req, self, Self::on_tick).await?,
            }
            Ok(())
        }

        pub fn on_tick(
            self,
            req: TickRequest,
        ) -> impl Stream<Item = TickResponse> + Send + 'static {
            let (tx, rx) = flume::bounded(2);
            tokio::task::spawn(async move {
                if let Err(err) = self.on_tick0(req, tx).await {
                    tracing::warn!(?err, "on_tick RPC handler failed");
                }
            });
            rx.into_stream()
        }

        pub async fn on_tick0(
            self,
            _req: TickRequest,
            tx: flume::Sender<TickResponse>,
        ) -> Result<()> {
            loop {
                let tick = *self.tick.read().unwrap();
                tx.send_async(TickResponse { tick }).await?;
                self.ontick.notified().await;
            }
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<S, C> {
        client: RpcClient<ClockService, S, C>,
    }

    impl<S, C> Client<S, C>
    where
        C: ServiceConnection<S>,
        S: Service,
    {
        pub fn new(client: RpcClient<ClockService, S, C>) -> Self {
            Self { client }
        }
        pub async fn tick(&self) -> Result<BoxStream<Result<usize>>> {
            let res = self.client.server_streaming(TickRequest).await?;
            Ok(res.map_ok(|r| r.tick).map_err(anyhow::Error::from).boxed())
        }
    }
}
