//! Generic channel factory, and some implementations
use std::{fmt, sync::Arc};

use crate::{ChannelTypes, Service};

/// A channel handle. Basically just a channel that is wrapped in an Arc
/// for cheap equality and cloning.
#[allow(type_alias_bounds)]
pub type ChannelHandle<S: Service, C: ChannelTypes> = Arc<C::Channel<S::Res, S::Req>>;

/// Result when attempting to open a channel
#[allow(type_alias_bounds)]
pub type CreateChannelResult<S: Service, C: ChannelTypes> =
    Result<ChannelHandle<S, C>, C::CreateChannelError>;

/// A channel factory provides channels and keeps track of channel errors.
///
/// It is informed when there are issues with the current channel and can then decide to
/// create a new channel and replace the old one.
pub trait ChannelFactory<S: Service, C: ChannelTypes>: fmt::Debug + Send + Sync + 'static {
    /// The current channel or reason why there is no channel.
    ///
    /// This method always returns immediately, even if there is no channel. It might trigger
    /// acquisition of a new channel in the background.
    fn update_channel(&self, target: &mut Option<ChannelHandle<S, C>>);

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn open_bi_error(&self, _channel: &ChannelHandle<S, C>, _error: &C::OpenBiError) {}

    /// Notification that there has been an error for the given channel. Depending on the error,
    /// this might indicate that the channel is no longer usable and a new one should be created.
    fn accept_bi_error(&self, _channel: &ChannelHandle<S, C>, _error: &C::AcceptBiError) {}
}

impl<S: Service, C: ChannelTypes> ChannelFactory<S, C> for Arc<dyn ChannelFactory<S, C>> {
    fn update_channel(&self, target: &mut Option<ChannelHandle<S, C>>) {
        (**self).update_channel(target)
    }

    fn open_bi_error(&self, channel: &ChannelHandle<S, C>, error: &C::OpenBiError) {
        (**self).open_bi_error(channel, error)
    }

    fn accept_bi_error(&self, channel: &ChannelHandle<S, C>, error: &C::AcceptBiError) {
        (**self).accept_bi_error(channel, error)
    }
}

impl<S: Service, C: ChannelTypes, T: ChannelFactory<S, C>> ChannelFactory<S, C> for Option<T> {
    fn update_channel(&self, target: &mut Option<ChannelHandle<S, C>>) {
        if let Some(this) = self {
            this.update_channel(target)
        }
    }

    fn open_bi_error(&self, channel: &ChannelHandle<S, C>, error: &C::OpenBiError) {
        if let Some(f) = self.as_ref() {
            f.open_bi_error(channel, error)
        }
    }

    fn accept_bi_error(&self, channel: &ChannelHandle<S, C>, error: &C::AcceptBiError) {
        if let Some(f) = self.as_ref() {
            f.accept_bi_error(channel, error)
        }
    }
}
