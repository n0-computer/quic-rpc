//! quic-rpc transports
//!
//! mem and combined are enabled by default. quic and http2 are enabled by feature flags.
pub mod combined;
pub mod mem;
#[cfg(feature = "quic")]
pub mod quinn;
#[cfg(feature = "http2")]
pub mod http2;

/// alias for quinn channel types
pub type QuinnChannelTypes = quinn::QuinnChannelTypes;
/// alias for http2 channel types
pub type Http2ChannelTypes = http2::Http2ChannelTypes;
/// alias for combined channel types
pub type CombinedChannelTypes<A, B> = combined::CombinedChannelTypes<A, B>;
/// alias for mem channel types
pub type MemChannelTypes = mem::MemChannelTypes;
