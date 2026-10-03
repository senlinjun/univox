//! Integration-test harness that boots a local TeamSpeak 3 server.
//!
//! The bundled free-license server enforces a single instance per machine
//! (shared-memory check), so [`Ts3Server::start`] serializes instances
//! through a global mutex that is held until the server is stopped.

use std::io::Write as _;
use std::net::{TcpListener, UdpSocket};
use std::path::{PathBuf};
use std::sync::Mutex as StdMutex;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;

static INSTANCE_LOCK: std::sync::LazyLock<std::sync::Arc<Mutex<()>>> =
    std::sync::LazyLock::new(|| std::sync::Arc::new(Mutex::new(())));

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf())
        .expect("test-support lives in <root>/crates")
}

fn default_server_dir() -> PathBuf {
    workspace_root().join("test/teamspeak3-server/teamspeak3-server_linux_amd64")
}

fn free_tcp_port() -> std::io::Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0")?;
    Ok(l.local_addr()?.port())
}

fn free_udp_port() -> std::io::Result<u16> {
    let s = UdpSocket::bind("127.0.0.1:0")?;
    Ok(s.local_addr()?.port())
}

#[derive(Debug, Clone, Default)]
pub struct Ts3ServerOptions {
    /// Path to the server installation directory.
    pub server_dir: Option<PathBuf>,
    /// Require the ServerAdmin privilege key to be present.
    pub require_token: bool,
    /// Bind the voice server to this fixed UDP port instead of a random one
    /// (for reconnect tests that restart the server on the same endpoint).
    pub voice_port: Option<u16>,
}

/// A running local TS3 server instance. Killing it on drop.
pub struct Ts3Server {
    child: tokio::process::Child,
    pub voice_port: u16,
    pub query_port: u16,
    pub query_ssh_port: u16,
    pub filetransfer_port: u16,
    pub serveradmin_password: String,
    pub admin_token: String,
    pub apikey: Option<String>,
    _tmpdir: PathBuf,
    _lock: tokio::sync::OwnedMutexGuard<()>,
    _lines: std::sync::Arc<StdMutex<Vec<String>>>,
}

impl Ts3Server {
    /// Boot a fresh server (new database, random ports) and wait until the
    /// ServerQuery port accepts connections.
    pub async fn start() -> Result<Self, String> {
        Self::start_with(Ts3ServerOptions::default()).await
    }

    pub async fn start_with(opts: Ts3ServerOptions) -> Result<Self, String> {
        let server_dir = opts
            .server_dir
            .clone()
            .or_else(|| std::env::var("UNIVOX_TS3_SERVER_DIR").ok().map(PathBuf::from))
            .unwrap_or_else(default_server_dir);
        if !server_dir.join("ts3server").exists() {
            return Err(format!(
                "ts3server binary not found in {}",
                server_dir.display()
            ));
        }

        let lock = INSTANCE_LOCK.clone().lock_owned().await;

        let tmpdir = std::env::temp_dir().join(format!(
            "univox-ts3-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmpdir).map_err(|e| e.to_string())?;
        // Bypass query flood protection for loopback.
        let mut allow = std::fs::File::create(tmpdir.join("query_ip_allowlist.txt"))
            .map_err(|e| e.to_string())?;
        allow
            .write_all(b"127.0.0.1\n::1\n")
            .map_err(|e| e.to_string())?;
        drop(allow);

        let voice_port = match opts.voice_port {
            Some(p) => p,
            None => free_udp_port().map_err(|e| e.to_string())?,
        };
        let filetransfer_port = free_tcp_port().map_err(|e| e.to_string())?;
        let query_port = free_tcp_port().map_err(|e| e.to_string())?;
        let query_ssh_port = free_tcp_port().map_err(|e| e.to_string())?;

        // Parameters use ServerQuery escaping; all our values are plain.
        let args = [
            "license_accepted=1".to_string(),
            // The HTTP query binds a fixed port (10080) that lingers across
            // rapid instance churn — disable it.
            "query_protocols=raw,ssh".to_string(),
            "clear_database=1".to_string(),
            format!("default_voice_port={voice_port}"),
            "voice_ip=127.0.0.1".to_string(),
            format!("filetransfer_port={filetransfer_port}"),
            "filetransfer_ip=127.0.0.1".to_string(),
            format!("query_port={query_port}"),
            "query_ip=127.0.0.1".to_string(),
            format!("query_ssh_port={query_ssh_port}"),
            "query_ssh_ip=127.0.0.1".to_string(),
            format!("dbsqlpath={}/", server_dir.join("sql").display()),
            "dbsqlcreatepath=create_sqlite/".to_string(),
            format!("logpath={}", tmpdir.join("logs").display()),
        ];

        let mut child = Command::new(server_dir.join("ts3server"))
            .args(&args)
            .current_dir(&tmpdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("failed to spawn ts3server: {e}"))?;

        // NOTE: the "I M P O R T A N T" banners with the serveradmin password,
        // apikey and the privilege key are printed on *stderr*.
        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");
        let lines: std::sync::Arc<StdMutex<Vec<String>>> = Default::default();
        for stream in [
            Box::new(stdout) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
            Box::new(stderr) as Box<dyn tokio::io::AsyncRead + Unpin + Send>,
        ] {
            let reader_lines = lines.clone();
            tokio::spawn(async move {
                let mut reader = BufReader::new(stream);
                let mut buf = String::new();
                loop {
                    buf.clear();
                    match reader.read_line(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            let line = buf.trim_end_matches(['\r', '\n']).to_string();
                            reader_lines.lock().unwrap().push(line);
                        }
                    }
                }
            });
        }

        // Wait for the query port to accept connections (server is up).
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        loop {
            if tokio::time::Instant::now() >= deadline {
                let _ = child.start_kill();
                return Err(format!(
                    "ts3server did not become ready in time; output:\n{}",
                    lines.lock().unwrap().join("\n")
                ));
            }
            if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                return Err(format!(
                    "ts3server exited during startup; output:\n{}",
                    lines.lock().unwrap().join("\n")
                ));
            }
            if tokio::net::TcpStream::connect(("127.0.0.1", query_port))
                .await
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }

        // The query port opens after the voice server binds, so the TCP
        // check above suffices; settle briefly.
        tokio::time::sleep(std::time::Duration::from_millis(1000)).await;

        let (password, token, apikey) = parse_credentials(&lines.lock().unwrap());
        if password.is_empty() {
            let _ = child.start_kill();
            return Err(format!(
                "could not parse serveradmin password from output:\n{}",
                lines.lock().unwrap().join("\n")
            ));
        }
        if opts.require_token && token.is_empty() {
            let _ = child.start_kill();
            return Err(format!(
                "no privilege token in output:\n{}",
                lines.lock().unwrap().join("\n")
            ));
        }

        Ok(Self {
            child,
            voice_port,
            query_port,
            query_ssh_port,
            filetransfer_port,
            serveradmin_password: password,
            admin_token: token,
            apikey,
            _tmpdir: tmpdir,
            _lock: lock,
            _lines: lines,
        })
    }

    /// Additional server output lines captured after start (for debugging).
    pub fn output_lines(&self) -> Vec<String> {
        self._lines.lock().unwrap().clone()
    }
}

impl Drop for Ts3Server {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_dir_all(&self._tmpdir);
    }
}

fn extract_between<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let idx = line.find(key)?;
    let rest = &line[idx + key.len()..];
    Some(rest.split('"').next()?.trim())
}

fn parse_credentials(lines: &[String]) -> (String, String, Option<String>) {
    let mut password = String::new();
    let mut token = String::new();
    let mut apikey = None;
    for line in lines {
        if password.is_empty() {
            if let Some(p) = extract_between(line, "password= \"") {
                password = p.to_string();
            }
        }
        if token.is_empty() {
            // stdout log lines carry a timestamp prefix; stderr banner lines
            // carry leading whitespace — match `token=` anywhere in the line.
            if let Some(idx) = line.find("token=") {
                token = line[idx + "token=".len()..].trim().to_string();
            }
        }
        if apikey.is_none() {
            if let Some(a) = extract_between(line, "apikey= \"") {
                apikey = Some(a.to_string());
            }
        }
    }
    (password, token, apikey)
}

/// Readiness helper: TCP connect to a host:port with timeout.
pub async fn wait_tcp<A: tokio::net::ToSocketAddrs>(
    addr: A,
    timeout: std::time::Duration,
) -> std::io::Result<tokio::net::TcpStream> {
    tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr))
        .await
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timeout"))?
}


#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn boots_server_and_parses_credentials() {
        let server = Ts3Server::start().await.expect("server start");
        assert!(!server.serveradmin_password.is_empty());
        assert!(!server.admin_token.is_empty(), "admin token should be parsed");
        assert_ne!(server.query_port, server.query_ssh_port);
        let _ = server.output_lines();
        let _ = workspace_root();
    }
}
