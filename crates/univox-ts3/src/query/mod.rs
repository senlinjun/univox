//! ServerQuery management driver (FEATURES.md §11.1).
//!
//! Second access path into a TS3 server: telnet-style TCP commands with
//! notifications bridged into the library event system. Reused verbatim by
//! TeamSpeak 6's management plane (its query command set is compatible).

pub mod connection;

pub use connection::{QueryConnection, QueryOptions};

use univox_ts3_proto::{Command, Result};

/// High-level ServerQuery client with typed helpers for the commands the
/// library relies on. Everything else goes through [`QuerySession::exec`].
pub struct QuerySession {
    conn: QueryConnection,
}

impl QuerySession {
    /// Connect, optionally login and select a virtual server.
    pub async fn connect(opts: QueryOptions) -> Result<Self> {
        Ok(Self {
            conn: QueryConnection::connect(opts).await?,
        })
    }

    /// Raw command execution: returns all response rows.
    pub async fn exec(&self, cmd: Command) -> Result<Vec<Vec<(String, String)>>> {
        self.conn.exec(cmd).await
    }

    /// Execute and return the first response row (common case).
    pub async fn exec_one(&self, cmd: Command) -> Result<Vec<(String, String)>> {
        Ok(self
            .exec(cmd)
            .await?
            .into_iter()
            .next()
            .unwrap_or_default())
    }

    pub fn notifications(&self) -> tokio::sync::broadcast::Receiver<Command> {
        self.conn.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        self.conn.is_closed()
    }

    pub async fn login(&self, username: &str, password: &str) -> Result<()> {
        self.exec(Command::new("login").pos(username).pos(password))
            .await
            .map(|_| ())
    }

    pub async fn logout(&self) -> Result<()> {
        self.exec(Command::new("logout")).await.map(|_| ())
    }

    pub async fn use_server(&self, sid: u64, virtual_server: bool) -> Result<()> {
        let mut cmd = Command::new("use").pos(sid.to_string());
        if virtual_server {
            cmd = cmd.opt("virtual");
        }
        self.exec(cmd).await.map(|_| ())
    }

    pub async fn whoami(&self) -> Result<Vec<(String, String)>> {
        self.exec_one(Command::new("whoami")).await
    }

    pub async fn version(&self) -> Result<Vec<(String, String)>> {
        self.exec_one(Command::new("version")).await
    }

    pub async fn send_text_message(
        &self,
        targetmode: u8,
        target: u64,
        message: &str,
    ) -> Result<()> {
        self.exec(
            Command::new("sendtextmessage")
                .param("targetmode", targetmode)
                .param("target", target)
                .param("msg", message),
        )
        .await
        .map(|_| ())
    }

    /// Register for notifications: event types
    /// `server|channel|textserver|textchannel|textprivate`. For `channel`,
    /// `id=0` selects ALL channels (omitting `id` errors on 3.13.x).
    pub async fn notify_register(&self, event: &str, id: Option<u64>) -> Result<()> {
        let mut cmd = Command::new("servernotifyregister").param("event", event);
        if let Some(id) = id {
            cmd = cmd.param("id", id);
        }
        self.exec(cmd).await.map(|_| ())
    }

    pub async fn notify_unregister(&self) -> Result<()> {
        self.exec(Command::new("servernotifyunregister"))
            .await
            .map(|_| ())
    }

    pub async fn quit(&self) -> Result<()> {
        self.conn.close().await
    }
}
