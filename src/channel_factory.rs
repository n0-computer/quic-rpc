//! Generic channel factory, and some implementations
use std::{
    fmt,
    sync::{Arc, Mutex},
};

use futures::{Future, FutureExt};

use crate::{ChannelTypes, RpcMessage};

/// Id to uniquely identify a channel from a factory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChannelId(pub u64);

/// A channel with an id to uniquely identify it
#[allow(type_alias_bounds)]
pub(crate) type NumberedChannel<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> =
    (C::Channel<In, Out>, ChannelId);

/// Result when attempting to open a channel
#[allow(type_alias_bounds)]
pub(crate) type CreateChannelResult<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> =
    Result<C::Channel<In, Out>, C::CreateChannelError>;

/// Result containing a numbered channel or an error
#[allow(type_alias_bounds)]
pub(crate) type ChannelOrError<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> =
    Result<NumberedChannel<In, Out, C>, C::CreateChannelError>;

#[allow(unused_variables)]
/// A channel factory provides channels and keeps track of channel errors.
///
/// It is informed when there are issues with the current channel and can then decide to
/// create a new channel and replace the old one.
pub trait ChannelFactory<In: RpcMessage, Out: RpcMessage, C: ChannelTypes>:
    fmt::Debug + Send + Sync + 'static
{
    /// Get a clone of the current channel.
    fn channel(&self) -> Option<NumberedChannel<In, Out, C>>;

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn open_bi_error(&self, id: ChannelId, error: &C::OpenBiError) {}

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn accept_bi_error(&self, id: ChannelId, error: &C::AcceptBiError) {}
}

pub(crate) struct ConstantChannelFactory<In: RpcMessage, Out: RpcMessage, C: ChannelTypes>(
    pub C::Channel<In, Out>,
);

impl<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> ChannelFactory<In, Out, C>
    for ConstantChannelFactory<In, Out, C>
{
    fn channel(&self) -> Option<NumberedChannel<In, Out, C>> {
        Some((self.0.clone(), ChannelId(0)))
    }
}

impl<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> fmt::Debug
    for ConstantChannelFactory<In, Out, C>
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ConstantChannelFactory")
    }
}

impl<In: RpcMessage, Out: RpcMessage, C: ChannelTypes> ChannelFactory<In, Out, C>
    for Arc<dyn ChannelFactory<In, Out, C>>
{
    fn channel(&self) -> Option<NumberedChannel<In, Out, C>> {
        (**self).channel()
    }

    fn open_bi_error(&self, id: ChannelId, error: &C::OpenBiError) {
        (**self).open_bi_error(id, error)
    }

    fn accept_bi_error(&self, id: ChannelId, error: &C::AcceptBiError) {
        (**self).accept_bi_error(id, error)
    }
}

struct Inner<In: RpcMessage, Out: RpcMessage, C: ChannelTypes, F> {
    /// function to attempt to create a channel
    f: F,
    /// current channel or channel creation failure reason, or None if no attempt has been made
    current: Option<ChannelOrError<In, Out, C>>,
    /// optional task to create a channel via the function `f`
    open: Option<tokio::task::JoinHandle<CreateChannelResult<In, Out, C>>>,
    /// counter to assign unique channel numbers
    counter: u64,
}

/// A channel factory that creates channels via a function.
pub struct LazyChannelFactory<In: RpcMessage, Out: RpcMessage, C: ChannelTypes, F>(
    Mutex<Inner<In, Out, C, F>>,
);

impl<In: RpcMessage, Out: RpcMessage, C: ChannelTypes, F> fmt::Debug
    for LazyChannelFactory<In, Out, C, F>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let inner = self.0.lock().unwrap();
        f.debug_struct("LazyChannelFactory")
            .field("current", &inner.current)
            .field("open", &inner.open.is_some())
            .finish()
    }
}

impl<In, Out, C, F, Fut> Inner<In, Out, C, F>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<In, Out, C>> + Send + 'static,
{
    /// Create a new inner, given the channel creation function `f`,
    /// an optional initial value `current`. Set `spawn` to true if you want to
    /// immediately spawn a task to create a channel.
    fn new(f: F, current: Option<CreateChannelResult<In, Out, C>>, spawn: bool) -> Self {
        let mut res = Self {
            f,
            current: current.map(|x| x.map(|e| (e, ChannelId(0)))),
            open: None,
            counter: 0,
        };
        if let Some(Ok(_)) = res.current.as_ref() {
            res.counter += 1;
        }
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
                    let id = ChannelId(self.counter);
                    self.counter += 1;
                    self.current = Some(Ok((channel, id)));
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

fn should_spawn_task<C, E>(
    target: &Option<Result<(C, ChannelId), E>>,
    source_id: ChannelId,
) -> bool {
    match target {
        // we got an error for our current channel, so we should try to get a new channel
        Some(Ok((_chan, id))) => *id == source_id,
        // we got either no channel or an error from the last task, so we should try again
        _ => true,
    }
}

impl<In, Out, C, F, Fut> ChannelFactory<In, Out, C> for LazyChannelFactory<In, Out, C, F>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<In, Out, C>> + Send + 'static,
{
    fn channel(&self) -> Option<NumberedChannel<In, Out, C>> {
        let mut inner = self.0.lock().unwrap();
        inner.update_current();
        if let Some(Ok((channel, id))) = &inner.current {
            Some((channel.clone(), *id))
        } else {
            None
        }
    }

    fn accept_bi_error(&self, channel: ChannelId, _error: &C::AcceptBiError) {
        let mut this = self.0.lock().unwrap();
        // does this error refer to the current channel?
        if should_spawn_task(&this.current, channel) {
            this.maybe_spawn_open();
        }
    }

    fn open_bi_error(&self, channel: ChannelId, _error: &C::OpenBiError) {
        let mut this = self.0.lock().unwrap();
        // does this error refer to the current channel?
        if should_spawn_task(&this.current, channel) {
            this.maybe_spawn_open();
        }
    }
}

impl<In: RpcMessage, Out: RpcMessage, C: ChannelTypes, F> Drop
    for LazyChannelFactory<In, Out, C, F>
{
    fn drop(&mut self) {
        let mut this = self.0.lock().unwrap();
        if let Some(open) = this.open.take() {
            open.abort();
        }
    }
}

impl<In, Out, C, F, Fut> LazyChannelFactory<In, Out, C, F>
where
    In: RpcMessage,
    Out: RpcMessage,
    C: ChannelTypes,
    F: Fn() -> Fut + Send + 'static,
    Fut: Future<Output = CreateChannelResult<In, Out, C>> + Send + 'static,
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
