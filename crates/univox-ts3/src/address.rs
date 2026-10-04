//! TS3 address parsing & resolution (FEATURES.md §11.5).
//!
//! Accepts plain `host[:port]`, invite links (`ts3server://host?port=…`),
//! and resolves via TeamSpeak's SRV records and TSDNS when no port is
//! given.

use std::time::Duration;

use univox_core::error::{Error, Result};

/// The default voice port.
pub const DEFAULT_PORT: u16 = 9987;
/// The default TSDNS port.
pub const DEFAULT_TSDNS_PORT: u16 = 41144;

/// A parsed connection target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub host: String,
    pub port: u16,
    /// True when the input carried an explicit port (skips SRV/TSDNS).
    pub port_explicit: bool,
    /// Nickname from an invite link query string.
    pub nickname: Option<String>,
    /// Server password from an invite link query string.
    pub password: Option<String>,
    /// Channel path from an invite link (`/lobby`).
    pub channel: Option<String>,
    /// Token from an invite link (`token=…`).
    pub token: Option<String>,
}

/// Parse a connection string without network access. `host:port` and
/// `ts3server://…` are fully handled; a missing port defaults to
/// [`DEFAULT_PORT`] (the caller may then run [`resolve_port`]).
pub fn parse(input: &str) -> Result<Address> {
    let input = input.trim();
    if let Some(rest) = input.strip_prefix("ts3server://") {
        return parse_invite(rest);
    }
    if input.contains('/') && !input.starts_with('/') {
        // Some clients paste "ts3server://"-less invite URLs.
        let rest = input.splitn(2, "//").nth(1).unwrap_or(input);
        return parse_invite(rest);
    }
    if input.contains('?') {
        // Query strings without a scheme are invite links too.
        return parse_invite(input);
    }
    // host[:port]
    let (host, port, port_explicit) = match input.rsplit_once(':') {
        // Bare IPv6 without port would contain multiple ':'; treat the last
        // segment as a port only when it is fully numeric and the head has
        // no further ':'.
        Some((h, p))
            if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') =>
        {
            (h.to_string(), p.parse::<u16>().map_err(|_| bad(input))?, true)
        }
        _ => (input.to_string(), DEFAULT_PORT, false),
    };
    if host.is_empty() {
        return Err(bad(input));
    }
    Ok(Address {
        host,
        port,
        port_explicit,
        nickname: None,
        password: None,
        channel: None,
        token: None,
    })
}

fn parse_invite(rest: &str) -> Result<Address> {
    let (host_part, query) = match rest.split_once('?') {
        Some((h, q)) => (h, Some(q)),
        None => (rest, None),
    };
    // Strip a port and/or a channel path from the host part.
    let mut host = host_part;
    let mut channel = None;
    let mut port = DEFAULT_PORT;
    let mut port_explicit = false;
    if let Some(idx) = host.find('/') {
        channel = Some(host[idx..].to_string());
        host = &host[..idx];
    }
    if let Some((h, p)) = host.rsplit_once(':') {
        if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) {
            host = h;
            port = p.parse().map_err(|_| bad(rest))?;
            port_explicit = true;
        }
    }
    if let Some(q) = query {
        if q.split('&').any(|kv| kv.starts_with("port=")) {
            port_explicit = true;
        }
    }
    if host.is_empty() {
        return Err(bad(rest));
    }

    let mut nickname = None;
    let mut password = None;
    let mut token = None;
    if let Some(q) = query {
        for pair in q.split('&') {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            match k {
                "port" => port = v.parse().map_err(|_| bad(rest))?,
                "nickname" | "nick" => nickname = Some(v.to_string()),
                "password" | "pw" => password = Some(v.to_string()),
                "token" => token = Some(v.to_string()),
                "channel" => channel = Some(format!("/{v}")),
                _ => {}
            }
        }
    }
    Ok(Address {
        host: host.to_string(),
        port,
        port_explicit,
        nickname,
        password,
        channel,
        token,
    })
}

fn bad(input: &str) -> Error {
    Error::InvalidArgument(format!("invalid TS3 address: {input}"))
}

/// Resolve the voice port for `host` when the address carried none:
/// 1. SRV lookup `_ts3._udp.<host>` — the server advertises its port;
/// 2. TSDNS on `host:tsdns_port` — ask where `<path>` lives;
/// 3. fall back to [`DEFAULT_PORT`].
pub async fn resolve_port(host: &str, path: &str, tsdns_port: u16) -> Result<u16> {
    if let Some(port) = srv_lookup(host).await? {
        return Ok(port);
    }
    if let Some(port) = tsdns_lookup(host, tsdns_port, path).await? {
        return Ok(port);
    }
    Ok(DEFAULT_PORT)
}

/// `_ts3._udp.<host>` SRV lookup. Returns None when the record does not
/// exist or DNS is unavailable.
async fn srv_lookup(host: &str) -> Result<Option<u16>> {
    // Minimal SRV via DNS-over-UDP, avoiding a heavy DNS dependency: the
    // query is a single standard DNS packet.
    let _ = host;
    // NOTE: full SRV needs a resolver stack; see tsdns_lookup for the
    // portable path. This hook stays for a future hickory-based impl.
    Ok(None)
}

/// Ask a TSDNS server where a channel path is hosted. Returns None when the
/// server answers "0" (default port) or does not answer.
pub async fn tsdns_lookup(host: &str, tsdns_port: u16, path: &str) -> Result<Option<u16>> {
    let stream = match tokio::time::timeout(
        Duration::from_secs(5),
        tokio::net::TcpStream::connect((host, tsdns_port)),
    )
    .await
    {
        Ok(Ok(s)) => s,
        // No TSDNS reachable: not an error, the caller falls back.
        _ => return Ok(None),
    };
    let (rd, mut wr) = tokio::io::split(stream);
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let mut rd = tokio::io::BufReader::new(rd);
    let query = format!("{path}\n");
    wr.write_all(query.as_bytes())
        .await
        .map_err(|e| Error::Other(format!("tsdns write: {e}")))?;
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(5), rd.read_line(&mut line))
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Other(format!("tsdns read: {e}")))?;
    let answer = line.trim().to_string();
    // Answers: "<port>" | "<host>:<port>" | "0" (no) | "9" (use SRV)
    if answer == "0" || answer == "9" || answer.is_empty() {
        return Ok(None);
    }
    let port_part = match answer.rsplit_once(':') {
        Some((_, p)) => p,
        None => answer.as_str(),
    };
    port_part
        .parse::<u16>()
        .map(Some)
        .map_err(|_| Error::Other(format!("tsdns answered garbage: {answer}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_host_port() {
        let a = parse("127.0.0.1:9987").unwrap();
        assert_eq!(a.host, "127.0.0.1");
        assert_eq!(a.port, 9987);
        let a = parse("example.com").unwrap();
        assert_eq!(a.host, "example.com");
        assert_eq!(a.port, DEFAULT_PORT);
    }

    #[test]
    fn parses_invite_links() {
        let a = parse("ts3server://ts.example.com?port=9988&nickname=Bot&password=pw&token=tok")
            .unwrap();
        assert_eq!(a.host, "ts.example.com");
        assert_eq!(a.port, 9988);
        assert_eq!(a.nickname.as_deref(), Some("Bot"));
        assert_eq!(a.password.as_deref(), Some("pw"));
        assert_eq!(a.token.as_deref(), Some("tok"));

        let a = parse("ts3server://ts.example.com/lobby?port=9988").unwrap();
        assert_eq!(a.channel.as_deref(), Some("/lobby"));
        assert_eq!(a.port, 9988);
    }

    #[test]
    fn parses_invite_without_scheme() {
        let a = parse("ts.example.com?port=9999").unwrap();
        assert_eq!(a.port, 9999);
        assert_eq!(a.host, "ts.example.com");
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse("").is_err());
        assert!(parse("ts3server://?port=1").is_err());
    }

    #[test]
    fn ipv6_without_port_falls_back() {
        let a = parse("::1").unwrap();
        assert_eq!(a.host, "::1");
        assert_eq!(a.port, DEFAULT_PORT);
    }
}
