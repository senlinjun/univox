//! ServerQuery connection over raw TCP (telnet-style line protocol).
//!
//! The actor owns the socket write half: commands are serialized (one in
//! flight, as the server expects), responses resolve their caller via
//! oneshot, and `notify*` lines are broadcast to subscribers.

use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, oneshot};

use univox_ts3_proto::{Command, Error, Result};

#[derive(Debug, Clone)]
pub struct QueryOptions {
    pub host: String,
    pub port: u16,
    pub username: Option<String>,
    /// Plaintext ServerQuery password (sent raw on this transport — the
    /// base64/SHA1 form is only used by the UDP client protocol).
    pub password: Option<String>,
    /// Virtual server to select after connecting (`use sid`).
    pub server: Option<u64>,
    /// Use `use sid -virtual` (select an offline server).
    pub use_virtual: bool,
    pub connect_timeout: Duration,
    /// Idle interval before sending a `whoami` keepalive. The server
    /// disconnects after `query_timeout` (default 300s) of inactivity.
    pub keepalive_interval: Duration,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".into(),
            port: 10011,
            username: None,
            password: None,
            server: None,
            use_virtual: false,
            connect_timeout: Duration::from_secs(10),
            keepalive_interval: Duration::from_secs(120),
        }
    }
}

pub type Rows = Vec<Vec<(String, String)>>;

struct Pending {
    rows: Rows,
    reply: oneshot::Sender<Result<Rows>>,
}

enum Request {
    Exec {
        cmd: Command,
        reply: oneshot::Sender<Result<Rows>>,
    },
    Shutdown(oneshot::Sender<()>),
}

/// A handle to the ServerQuery connection actor.
#[derive(Clone)]
pub struct QueryConnection {
    tx: mpsc::Sender<Request>,
    notify_tx: broadcast::Sender<Command>,
    closed: Arc<tokio::sync::watch::Receiver<bool>>,
}

/// Detect the server's per-command flood protection. The error id differs
/// across versions (`3331`/`524`/...); the message is stable
/// ("client is flooding").
fn is_flood_error(e: &Error) -> Option<Duration> {
    match e {
        Error::Server { msg, extra, .. } if msg.contains("flooding") => {
            // extra_msg looks like "please wait 1 seconds".
            let wait = extra
                .iter()
                .find(|(k, _)| k == "extra_msg")
                .and_then(|(_, v)| v.split_whitespace().last())
                .and_then(|w| w.parse::<u64>().ok())
                .unwrap_or(1);
            Some(Duration::from_secs(wait))
        }
        _ => None,
    }
}

impl QueryConnection {
    pub async fn connect(opts: QueryOptions) -> Result<Self> {
        let stream = tokio::time::timeout(
            opts.connect_timeout,
            TcpStream::connect((opts.host.as_str(), opts.port)),
        )
        .await
        .map_err(|_| Error::Timeout)?
        ?;
        stream.set_nodelay(true).ok();
        let (read_half, write_half) = stream.into_split();

        let (notify_tx, _) = broadcast::channel(256);
        let (req_tx, req_rx) = mpsc::channel(64);
        let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);

        tokio::spawn(run_actor(
            read_half,
            write_half,
            req_rx,
            notify_tx.clone(),
            closed_tx,
            opts.keepalive_interval,
        ));

        let conn = Self {
            tx: req_tx,
            notify_tx,
            closed: Arc::new(closed_rx),
        };

        // Validate the connection with a probe command (the greeting lines
        // "TS3"/welcome are skipped inside the actor).
        conn.exec(Command::new("version")).await?;

        if let Some(user) = &opts.username {
            conn.exec(
                Command::new("login")
                    .pos(user.clone())
                    .pos(opts.password.clone().unwrap_or_default()),
            )
            .await?;
        }
        if let Some(sid) = opts.server {
            let mut cmd = Command::new("use").pos(sid.to_string());
            if opts.use_virtual {
                cmd = cmd.opt("virtual");
            }
            conn.exec(cmd).await?;
        }
        Ok(conn)
    }

    /// Execute one command, returning all response rows. Retries with
    /// backoff when the server flood protection intervenes.
    pub async fn exec(&self, cmd: Command) -> Result<Rows> {
        const MAX_RETRIES: u32 = 3;
        let mut retries = 0u32;
        loop {
            match self.exec_once(&cmd).await {
                Err(e) if retries < MAX_RETRIES => {
                    if let Some(wait) = is_flood_error(&e) {
                        retries += 1;
                        tracing::warn!(attempt = retries, ?wait, "query flood limit hit, backing off");
                        tokio::time::sleep(wait + Duration::from_millis(100)).await;
                    } else {
                        return Err(e);
                    }
                }
                other => return other,
            }
        }
    }

    async fn exec_once(&self, cmd: &Command) -> Result<Rows> {
        if *self.closed.borrow() {
            return Err(Error::Closed);
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Request::Exec {
                cmd: cmd.clone(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| Error::Closed)?;
        match tokio::time::timeout(Duration::from_secs(30), reply_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::Closed),
            Err(_) => Err(Error::Timeout),
        }
    }

    /// Subscribe to `notify*` notification lines.
    pub fn subscribe(&self) -> broadcast::Receiver<Command> {
        self.notify_tx.subscribe()
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    /// Graceful goodbye (`quit`). Waits until the actor confirms closure.
    pub async fn close(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Request::Shutdown(tx)).await.is_err() {
            return Ok(());
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), rx).await;
        // Wait for the actor to observe the server's reply / EOF.
        let mut closed = self.closed.as_ref().clone();
        let _ = closed.wait_for(|v| *v).await;
        Ok(())
    }
}

async fn run_actor(
    read_half: OwnedReadHalf,
    write_half: OwnedWriteHalf,
    mut rx: mpsc::Receiver<Request>,
    notify_tx: broadcast::Sender<Command>,
    closed_tx: tokio::sync::watch::Sender<bool>,
    keepalive: Duration,
) {
    // Reader task: lines in, channel out.
    let (line_tx, mut line_rx) = mpsc::channel::<Result<String>>(64);
    tokio::spawn(async move {
        let mut reader = BufReader::with_capacity(64 * 1024, read_half);
        let mut buf = Vec::with_capacity(4096);
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    // The server prefixes most lines with a bare `\r` and
                    // terminates with `\n`; strip both before classification.
                    let raw = String::from_utf8_lossy(&buf);
                    let line = raw.trim_matches(['\r', '\n']);
                    if line_tx.send(Ok(line.to_string())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = line_tx.send(Err(Error::Closed)).await;
    });

    let mut write_half = write_half;
    let mut pending: Option<Pending> = None;
    let mut last_activity = Instant::now();
    let mut shutting_down = false;

    loop {
        // Keepalive tick: only when we have been fully idle.
        let idle = last_activity.elapsed();
        let tick = if idle >= keepalive {
            Duration::ZERO
        } else {
            keepalive - idle
        };
        tokio::select! {
            biased;
            line = line_rx.recv() => {
                let line = match line {
                    Some(Ok(l)) => l,
                    Some(Err(_)) | None => break,
                };
                last_activity = Instant::now();
                match classify(&line) {
                    Line::Greeting => {}
                    Line::Error => {
                        let err = parse_error_line(&line);
                        if err.server_id() == Some(0) {
                            if let Some(p) = pending.take() {
                                let _ = p.reply.send(Ok(p.rows));
                            }
                            if shutting_down {
                                break;
                            }
                        } else if let Some(p) = pending.take() {
                            let _ = p.reply.send(Err(err));
                        }
                    }
                    Line::Notify => match Command::parse(&line) {
                        Ok(cmd) => {
                            let _ = notify_tx.send(cmd);
                        }
                        Err(e) => tracing::warn!(%line, error = %e, "bad notify line"),
                    },
                    Line::Row => match &mut pending {
                        Some(p) => match Command::parse(&line) {
                            // One wire line can carry many rows (`|`-separated).
                            Ok(cmd) => p.rows.extend(cmd.params),
                            Err(e) => tracing::warn!(%line, error = %e, "bad row line"),
                        },
                        None => tracing::warn!(%line, "unexpected data line outside command"),
                    },
                }
            }
            req = rx.recv() => {
                let req = match req {
                    Some(r) => r,
                    None => break,
                };
                last_activity = Instant::now();
                match req {
                    Request::Exec { cmd, reply } => {
                        if pending.is_some() {
                            let _ = reply.send(Err(Error::Other(
                                "concurrent query command (internal bug)".into(),
                            )));
                            continue;
                        }
                        if write_half
                            .write_all(format!("{}\n", cmd.serialize()).as_bytes())
                            .await
                            .is_err()
                        {
                            let _ = reply.send(Err(Error::Closed));
                            break;
                        }
                        pending = Some(Pending { rows: Vec::new(), reply });
                    }
                    Request::Shutdown(done) => {
                        shutting_down = true;
                        let _ = write_half.write_all(b"quit\n").await;
                        let _ = done.send(());
                        // Wait briefly for the error line / EOF via the loop.
                    }
                }
            }
            _ = tokio::time::sleep(tick) => {
                // Never interleave with an in-flight command: the whoami
                // response would be attributed to it.
                if pending.is_none() {
                    let _ = write_half.write_all(b"whoami\n").await;
                    let (tx, _rx) = oneshot::channel();
                    pending = Some(Pending { rows: Vec::new(), reply: tx });
                }
                last_activity = Instant::now();
            }
        }
    }
    let _ = closed_tx.send(true);
}

enum Line {
    Greeting,
    Error,
    Notify,
    Row,
}

fn classify(line: &str) -> Line {
    if line.is_empty() || line == "TS3" || line.starts_with("Welcome to the TeamSpeak 3") {
        return Line::Greeting;
    }
    if line.starts_with("error ") || line == "error" {
        return Line::Error;
    }
    if line.starts_with("notify") {
        return Line::Notify;
    }
    Line::Row
}

fn parse_error_line(line: &str) -> Error {
    match Command::parse(line) {
        Ok(cmd) => {
            let id = cmd.get("id").and_then(|v| v.parse().ok()).unwrap_or(-1);
            let msg = cmd.get("msg").unwrap_or("").to_string();
            let extra = cmd
                .params
                .first()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|(k, _)| k != "id" && k != "msg")
                .collect();
            Error::Server { id, msg, extra }
        }
        Err(_) => Error::Parse(format!("bad error line: {line:?}")),
    }
}
