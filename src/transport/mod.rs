//! Transports for quic-rpc
//!
//! mem and combined are enabled by default. quic and http2 are enabled by feature flags.
pub mod combined;
#[cfg(feature = "http2")]
pub mod http2;
pub mod mem;
#[cfg(feature = "quic")]
pub mod quinn;

#[cfg(feature = "quic")]
pub mod s2n_quic;

#[cfg(feature = "quic")]
/// Alias for quinn channel types
pub use self::quinn::QuinnChannelTypes;
#[cfg(feature = "quic")]
/// Alias for s2n_quic channel types
pub use self::s2n_quic::ChannelTypes as S2nQuicChannelTypes;
/// Alias for combined channel types
pub use combined::CombinedChannelTypes;
#[cfg(feature = "http2")]
/// Alias for http2 channel types
pub use http2::Http2ChannelTypes;
/// Alias for mem channel types
pub use mem::MemChannelTypes;
