//! Optional device transport. Constructing a client never opens a connection.
mod bridge;
mod client;
mod continuity;
mod error;
mod handoff;
pub use continuity::*;
pub use error::*;
mod oauth;
mod placement;
pub use handoff::*;
pub use placement::*;
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
