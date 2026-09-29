//! TS3 error types shared across protocol layers.

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("server error {id}: {msg}")]
    Server {
        id: i32,
        msg: String,
        extra: Vec<(String, String)>,
    },
    #[error("parse error: {0}")]
    Parse(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("crypto error: {0}")]
    Crypto(String),
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("connection closed")]
    Closed,
    #[error("timeout")]
    Timeout,
    #[error("{0}")]
    Other(String),
}

impl Error {
    /// Server error id if this is a [`Error::Server`].
    pub fn server_id(&self) -> Option<i32> {
        match self {
            Error::Server { id, .. } => Some(*id),
            _ => None,
        }
    }

    pub fn is_server_err(&self, id: i32) -> bool {
        self.server_id() == Some(id)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// Common TS3 server error ids (subset; see serverquery docs `error id=`).
pub mod ids {
    pub const OK: i32 = 0;
    pub const COMMAND_NOT_FOUND: i32 = 256;
    pub const LOGIN_FAILED: i32 = 520;
    pub const INVALID_PASSWORD: i32 = 522;
    pub const ALREADY_LOGGED_IN: i32 = 513;
    pub const NOT_LOGGED_IN: i32 = 524;
    pub const INVALID_SERVER_ID: i32 = 1024;
    pub const SERVER_IS_NOT_RUNNING: i32 = 1033;
    pub const CLIENT_INVALID_ID: i32 = 512;
    pub const CHANNEL_INVALID_ID: i32 = 768;
    pub const PERMISSIONS_CLIENT_INSUFFICIENT: i32 = 2568;
    pub const BAN_FLOODING: i32 = 3331;
    pub const CLIENT_IS_FLOODING: i32 = 3332;
    pub const BANNED: i32 = 3333;
}
