//! Predefined interaction patterns.
//!
//! An interaction pattern can be as simple as an rpc call or something more
//! complex such as bidirectional streaming.
//!
//! Each pattern defines different associated message types for the interaction.
pub mod bidi_streaming;
pub mod client_streaming;
pub mod rpc;
pub mod server_streaming;
pub mod try_server_streaming;
