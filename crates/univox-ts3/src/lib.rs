//! TeamSpeak 3 driver for Univox.
//!
//! Two access paths:
//! - [`query`]: ServerQuery management driver (telnet-style TCP).
//! - client protocol (UDP voice + control) — under construction.

pub mod book;
pub mod client;
pub mod query;
pub mod session;

pub use client::{HandshakeOptions, UdpConnection};
pub use query::{QueryConnection, QueryOptions, QuerySession};
pub use session::{map_proto_err, self_clid, ts3_capabilities, Ts3Driver, Ts3Session};
