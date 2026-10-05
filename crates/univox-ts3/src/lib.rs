//! TeamSpeak 3 driver for Univox.
//!
//! Two access paths:
//! - [`query`]: ServerQuery management driver (telnet-style TCP).
//! - client protocol (UDP voice + control) — under construction.

pub mod address;
pub mod book;
pub mod client;
pub mod ext;
pub mod filetransfer;
pub use filetransfer::{FileDownload, FileUpload, TransferChannel};
pub mod query;
pub mod session;

pub use client::{HandshakeOptions, UdpConnection};
pub use query::{QueryConnection, QueryOptions, QuerySession};
pub use ext::{
    avatar_path, ChannelGroup, ClientDbEntry, ClientMatch, SelfUpdate, ServerGroup, Ts3Ext,
};
pub use session::{
    map_proto_err, self_clid, ts3_capabilities, Ts3ConnectOptions, Ts3Driver, Ts3Session,
};
