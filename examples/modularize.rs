use quic_rpc::{transport::flume, RpcServer};
use tracing::warn;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (server_conn, client_conn) = flume::connection::<app::Request, app::Response>(1);

    tokio::task::spawn(async move {
        let server = RpcServer::new(server_conn);
        let handler = app::Handler::default();
        loop {
            match server.accept().await {
                Err(err) => warn!(?err, "server accept failed"),
                Ok((req, chan)) => {
                    let handler = handler.clone();
                    tokio::task::spawn(handler.handle_rpc_request(req, chan));
                }
            }
        }
    });

    app::client_demo(client_conn).await?;

    Ok(())
}

mod app {
    use anyhow::Result;
    use derive_more::{From, TryInto};
    use futures::StreamExt;
    use quic_rpc::{server::RpcChannel, RpcClient, ServiceConnection, ServiceEndpoint};
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
    pub struct AppService;
    impl quic_rpc::Service for AppService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone, Default)]
    pub struct Handler {
        calc: calc::Handler,
        clock: clock::Handler,
    }

    impl Handler {
        pub async fn handle_rpc_request<E: ServiceEndpoint<AppService>>(
            self,
            req: Request,
            chan: RpcChannel<AppService, E>,
        ) {
            let _res = match req {
                Request::Calc(req) => self.calc.handle_rpc_request(req, chan.map()).await,
                Request::Clock(req) => self.clock.handle_rpc_request(req, chan.map()).await,
            };
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<C: ServiceConnection<AppService>> {
        pub calc: calc::Client<C, AppService>,
        pub clock: clock::Client<C, AppService>,
    }

    impl<C: ServiceConnection<AppService>> Client<C> {
        pub fn new(conn: C) -> Self {
            let client = RpcClient::new(conn);
            Self {
                calc: calc::Client::new(client.clone()),
                clock: clock::Client::new(client.clone()),
            }
        }
    }

    pub async fn client_demo<C: ServiceConnection<AppService>>(conn: C) -> Result<()> {
        let client = Client::new(conn);
        println!("calc service: add");
        let res = client.calc.add(40, 2).await?;
        println!("calc service: res {res:?}");
        println!("clock service: start tick");
        let mut stream = client.clock.tick().await?;
        while let Some(tick) = stream.next().await {
            let tick = tick?;
            println!("clock service: tick {tick}");
        }
        Ok(())
    }
}

mod calc {
    use derive_more::{From, TryInto};
    use quic_rpc::{
        message::RpcMsg, server::RpcChannel, IntoService, RpcClient, ServiceConnection,
        ServiceEndpoint,
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

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Request {
        Add(AddRequest),
    }

    #[derive(Debug, Serialize, Deserialize, From, TryInto)]
    pub enum Response {
        Add(AddResponse),
    }

    #[derive(Copy, Clone, Debug)]
    pub struct CalcService;
    impl quic_rpc::Service for CalcService {
        type Req = Request;
        type Res = Response;
    }

    #[derive(Clone, Default)]
    pub struct Handler;

    impl Handler {
        pub async fn handle_rpc_request<S, E>(
            self,
            req: Request,
            chan: RpcChannel<S, E, CalcService>,
        ) where
            S: IntoService<CalcService>,
            E: ServiceEndpoint<S>,
        {
            let _res = match req {
                Request::Add(req) => chan.rpc(req, self, Self::on_add).await,
            };
        }

        pub async fn on_add(self, req: AddRequest) -> AddResponse {
            AddResponse(req.0 + req.1)
        }
    }

    #[derive(Debug, Clone)]
    pub struct Client<C, S = CalcService> {
        client: RpcClient<S, C, CalcService>,
    }

    impl<C, S> Client<C, S>
    where
        C: ServiceConnection<S>,
        S: IntoService<CalcService>,
    {
        pub fn new(client: RpcClient<S, C>) -> Self {
            Self {
                client: client.map(),
            }
        }
        pub async fn add(&self, a: i64, b: i64) -> anyhow::Result<i64> {
            let res = self.client.rpc(AddRequest(a, b)).await?;
            Ok(res.0)
        }
    }
}

mod clock {
    use anyhow::Result;
    use derive_more::{From, TryInto};
    use futures::{stream::BoxStream, Stream, StreamExt, TryStreamExt};
    use quic_rpc::{
        message::{Msg, ServerStreaming, ServerStreamingMsg},
        server::RpcChannel,
        IntoService, RpcClient, ServiceConnection, ServiceEndpoint,
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
    impl quic_rpc::Service for ClockService {
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
            chan: RpcChannel<S, E, ClockService>,
        ) where
            S: IntoService<ClockService>,
            E: ServiceEndpoint<S>,
        {
            let _res = match req {
                Request::Tick(req) => chan.server_streaming(req, self, Self::on_tick).await,
            };
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
    pub struct Client<C, S = ClockService> {
        client: RpcClient<S, C, ClockService>,
    }

    impl<C, S> Client<C, S>
    where
        C: ServiceConnection<S>,
        S: IntoService<ClockService>,
    {
        pub fn new(client: RpcClient<S, C>) -> Self {
            Self {
                client: client.map(),
            }
        }
        pub async fn tick(&self) -> Result<BoxStream<'static, Result<usize>>> {
            let res = self.client.server_streaming(TickRequest).await?;
            Ok(res.map_ok(|r| r.tick).map_err(anyhow::Error::from).boxed())
        }
    }
}
