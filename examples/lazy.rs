use std::{fmt::Debug, sync::Arc};

use futures::{FutureExt, Future};
use parking_lot::Mutex;
use quic_rpc::{client::ChannelFactory, ChannelTypes, Service, mem::{MemChannelTypes, self}, RpcClient};

struct Inner<S: Service, C: ChannelTypes, F> {
    f: F,
    current: Result<C::Channel<S::Res, S::Req>, quic_rpc::OpenChannelError>,
    open: Option<tokio::task::JoinHandle<OpenResult<S,C>>>,
}

#[allow(type_alias_bounds)]
type OpenResult<S: Service, C: ChannelTypes> = Result<C::Channel<S::Res, S::Req>, quic_rpc::OpenChannelError>;

struct LazyChannelFactory<S: Service, C: ChannelTypes, F>(Mutex<Inner<S, C, F>>);

impl<S: Service, C: ChannelTypes, F> Debug for LazyChannelFactory<S, C, F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LazyChannelHolder")
            .finish()
    }
}

impl<S: Service, C: ChannelTypes, F, Fut> Inner<S, C, F> 
    where
        S: Service,
        C: ChannelTypes,
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = OpenResult<S,C>> + Send + 'static,
{
    fn new(f: F, current: OpenResult<S,C>, spawn: bool) -> Self {
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
                    self.current = Ok(channel);
                }
                Ok(Err(e)) => {
                    // the open task failed, so we can't use the channel
                    // should we use the new or the old error?
                    self.current = Err(e);
                    self.maybe_spawn_open();
                }
                Err(_) => {
                    // the open task panicked, so we can't use the channel
                    // should we use the new or the old error?
                    self.current = Err(quic_rpc::OpenChannelError::NoChannel);
                    self.maybe_spawn_open();
                }
            }
        }
    }

    fn maybe_spawn_open(&mut self) {
        if self.open.is_none() {
            self.open = Some(tokio::spawn((self.f)()));
        }
    }
}

impl<S: Service, C: ChannelTypes, F, Fut> ChannelFactory<S, C> for LazyChannelFactory<S, C, F>
    where
        S: Service,
        C: ChannelTypes,
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = Result<C::Channel<S::Res, S::Req>, quic_rpc::OpenChannelError>> + Send + 'static,
{
    fn current(&self) -> Result<C::Channel<S::Res, S::Req>, quic_rpc::OpenChannelError> {
        let mut this = self.0.lock();
        this.update_current();
        this.current.clone()
    }

    fn accept_bi_error(&self, _channel: &C::Channel<S::Res, S::Req>, _error: &C::AcceptBiError) {
        let mut this = self.0.lock();
        this.maybe_spawn_open();
    }

    fn open_bi_error(&self, _channel: &C::Channel<S::Res, S::Req>, _error: &C::OpenBiError) {
        let mut this = self.0.lock();
        this.maybe_spawn_open();
    }
}


impl<S: Service, C: ChannelTypes, F, Fut> LazyChannelFactory<S, C, F> 
    where
        S: Service,
        C: ChannelTypes,
        F: Fn() -> Fut + Send + 'static,
        Fut: Future<Output = OpenResult<S,C>> + Send + 'static,
{
    pub fn lazy(f: F) -> Self {
        Self(Mutex::new(Inner::new(f, Err(quic_rpc::OpenChannelError::NoChannel), true)))
    }

    pub fn super_lazy(f: F) -> Self {
        Self(Mutex::new(Inner::new(f, Err(quic_rpc::OpenChannelError::NoChannel), false)))
    }

    pub async fn eager(f: F) -> Self {
        let current = f().await;
        Self(Mutex::new(Inner::new(f, current, true)))
    }
}

#[derive(Debug, Clone)]
struct DummyService;

impl Service for DummyService {
    type Req = ();
    type Res = ();
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    type S = DummyService;
    type C = MemChannelTypes;
    let (c, s) = mem::connection(1);
    let cf = LazyChannelFactory::<S,C, _>::eager(move || {
        let c = c.clone();
        async move {
            Ok(c)
        }}).await;
    let client: RpcClient<S, C> = quic_rpc::client::RpcClient::lazy(Arc::new(cf));
    Ok(())
}
