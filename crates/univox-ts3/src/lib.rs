//! TeamSpeak 3 driver for Univox.
//!
//! Two access paths:
//! - [`query`]: ServerQuery management driver (telnet-style TCP).
//! - client protocol (UDP voice + control) — under construction.

pub mod query;

pub use query::{QueryConnection, QueryOptions, QuerySession};
