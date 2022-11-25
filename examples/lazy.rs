use std::{
    fmt::Debug,
    sync::{Arc, Mutex},
};

use futures::{Future, FutureExt};
use quic_rpc::{
    channel_factory::{ChannelFactory, ChannelHandle, CreateChannelResult},
    mem::{self, MemChannelTypes},
    rpc_service, ChannelTypes, RpcClient, RpcServer, Service,
};
use serde::{Deserialize, Serialize};

struct Inner<S: Service, C: ChannelTypes, F> {
    /// function to attempt to create a channel
    f: F,
    /// current channel or channel creation failure reason, or None if no attempt has been made
    current: Option<CreateChannelResult<S, C>>,
    /// optional task to create a channel via the function `f`
    open: Option<tokio::task::JoinHandle<CreateChannelResult<S, C>>>,
}

struct LazyChannelFactory<S: Service, C: ChannelTypes, F>(Mutex<Inner<S, C, F>>);

impl<S: Service, C: ChannelTypes, F> Debug for LazyChannelFactory<S, C, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.0.lock().unwrap();
        f.debug_struct("LazyChannelFactory")
            .field("current", &inner.current)
            .field("open", &inner.open.is_some())
            .finish()
    }
}

impl<S: Service, C: ChannelTypes, F, Fut> Inner<S, C, F>
where
    S: Service,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<S, C>> + Send + 'static,
{
    /// Create a new inner, given the channel creation function `f`,
    /// an optional initial value `current`. Set `spawn` to true if you want to
    /// immediately spawn a task to create a channel.
    fn new(f: F, current: Option<CreateChannelResult<S, C>>, spawn: bool) -> Self {
        let mut res = Self {
            f,
            current,
            open: None,
        };
        if spawn {
            res.maybe_spawn_open();
        }
        res
    }

    /// Check if the open task is finished and if so, update the current channel
    fn update_current(&mut self) {
        // if finished is true, we do have a task and it is finished
        let finished = self
            .open
            .as_ref()
            .map(|t| t.is_finished())
            .unwrap_or_default();
        if finished {
            // get the result of the open task
            let result = self
                .open
                .take()
                .expect("task must exist")
                .now_or_never()
                .expect("task must be finished");
            match result {
                Ok(Ok(channel)) => {
                    // the open task succeeded, so we can use the channel
                    self.current = Some(Ok(channel));
                }
                Ok(Err(e)) => {
                    // the open task failed, so we can't use the channel
                    // should we use the new or the old error?
                    self.current = Some(Err(e));
                    self.maybe_spawn_open();
                }
                Err(_) => {
                    // the open task panicked, so we can't use the channel
                    self.current = None;
                    self.maybe_spawn_open();
                }
            }
        }
    }

    /// Spawn a task to open a channel if there is not already one
    fn maybe_spawn_open(&mut self) {
        if self.open.is_none() {
            self.open = Some(tokio::spawn((self.f)()));
        }
    }
}

fn should_update<T>(target: &Option<Arc<T>>, source: &Arc<T>) -> bool {
    match target {
        Some(target) => !Arc::ptr_eq(target, source),
        None => true,
    }
}

fn is_current_channel<T, U>(target: &Option<Result<Arc<T>, U>>, source: &Arc<T>) -> bool {
    match target {
        Some(Ok(target)) => !Arc::ptr_eq(target, source),
        _ => true,
    }
}

impl<S: Service, C: ChannelTypes, F, Fut> ChannelFactory<S, C> for LazyChannelFactory<S, C, F>
where
    S: Service,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<S, C>> + Send + 'static,
{
    fn update_channel(&self, target: &mut Option<ChannelHandle<S, C>>) {
        let mut this = self.0.lock().unwrap();
        this.update_current();
        if let Some(Ok(channel)) = &this.current {
            if should_update(target, channel) {
                *target = Some(channel.clone());
            }
        }
    }

    fn accept_bi_error(&self, channel: &ChannelHandle<S, C>, _error: &C::AcceptBiError) {
        let mut this = self.0.lock().unwrap();
        // does this error refer to the current channel?
        if is_current_channel(&this.current, channel) {
            this.maybe_spawn_open();
        }
    }

    fn open_bi_error(&self, channel: &ChannelHandle<S, C>, _error: &C::OpenBiError) {
        let mut this = self.0.lock().unwrap();
        // does this error refer to the current channel?
        if is_current_channel(&this.current, channel) {
            this.maybe_spawn_open();
        }
    }
}

impl<S: Service, C: ChannelTypes, F> Drop for LazyChannelFactory<S, C, F> {
    fn drop(&mut self) {
        let mut this = self.0.lock().unwrap();
        if let Some(open) = this.open.take() {
            open.abort();
        }
    }
}

impl<S: Service, C: ChannelTypes, F, Fut> LazyChannelFactory<S, C, F>
where
    S: Service,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<S, C>> + Send + 'static,
{
    /// Channel will be created immediately in the background
    ///
    /// This means that the first request using this client might fail,
    /// if you use it before the background task has completed.
    pub fn lazy(f: F) -> Self {
        Self(Mutex::new(Inner::new(f, None, true)))
    }

    /// Channel will be created in the background at the time of first use
    ///
    /// This means that the first request using this client *will* fail even if
    /// channel creation is successful.
    pub fn super_lazy(f: F) -> Self {
        Self(Mutex::new(Inner::new(f, None, false)))
    }

    /// Channel will be created immediately in the foreground
    ///
    /// This method returns once the channel has been created, or creation has
    /// failed. If the channel creation was successful, the first request using
    /// this client will not fail.
    pub async fn eager(f: F) -> Self {
        let current = f().await;
        Self(Mutex::new(Inner::new(f, Some(current), true)))
    }
}

// define a tiny test service
#[derive(Debug, Serialize, Deserialize)]
pub struct PingRequest;

#[derive(Debug, Serialize, Deserialize)]
pub struct PingResponse;

rpc_service! {
    Request = TestRequest;
    Response = TestResponse;
    Service = TestService;
    CreateDispatch = _;
    CreateClient = _;

    Rpc ping = PingRequest, _ -> PingResponse;
}

struct PingServer;

impl PingServer {
    async fn ping(self, _req: PingRequest) -> PingResponse {
        PingResponse
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    type S = TestService;
    type C = MemChannelTypes;
    let (c, s) = mem::connection(1);
    // start the test service
    let server_handle = tokio::task::spawn(async move {
        let s = RpcServer::<S, C>::new(s);
        loop {
            let (req, chan) = s.accept_one().await?;
            match req {
                TestRequest::PingRequest(req) => {
                    s.rpc(req, chan, PingServer, PingServer::ping).await
                }
            }?
        }
        anyhow::Ok(())
    });
    let cf = LazyChannelFactory::<S, C, _>::super_lazy(move || {
        let c = c.clone();
        async move { Ok(Arc::new(c)) }
    });
    let mut client: RpcClient<S, C> = quic_rpc::client::RpcClient::from_factory(Arc::new(cf));
    let res = client.rpc(PingRequest).await?;
    Ok(())
}
