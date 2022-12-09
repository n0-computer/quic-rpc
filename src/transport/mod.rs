//! Transports for quic-rpc
//!
//! mem and combined are enabled by default. quic and http2 are enabled by feature flags.
pub mod combined;
#[cfg(feature = "http2")]
pub mod http2;
pub mod mem;
#[cfg(feature = "quic")]
pub mod quinn;

/// Alias for quinn channel types
pub use self::quinn::ChannelTypes as QuinnChannelTypes;
/// Alias for combined channel types
pub use combined::ChannelTypes as CombinedChannelTypes;
/// Alias for http2 channel types
pub use http2::ChannelTypes as Http2ChannelTypes;
/// Alias for mem channel types
pub use mem::ChannelTypes as MemChannelTypes;
