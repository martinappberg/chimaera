//! Optional device transport. Constructing a client never opens a connection.
mod bridge;
mod client;
mod handoff;
mod oauth;
pub use handoff::*;
pub mod protocol;
mod transport;
pub use bridge::{bridge, websocket_config};
pub use client::{Client, EventConnection, LinkTunnel, Serve};
pub use oauth::Pkce;
pub use protocol::*;
pub use transport::Socket;
#[cfg(feature = "fixtures")]
pub mod conformance;
#[cfg(feature = "fixtures")]
pub mod fake;

#[cfg(feature = "fixtures")]
mod fake_handoff;
