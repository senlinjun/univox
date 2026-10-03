//! Native TS3 client protocol (UDP voice + control).

pub mod connection;

pub use connection::{HandshakeOptions, Rows, UdpConnection};

use std::sync::Arc;
use std::time::Duration;

use univox_ts3_proto as proto;
use univox_ts3_proto::Identity;
use univox_ts3_proto::{Command, Error};

/// One connection attempt: fresh socket, `identity` + matching
/// `client_key_offset`. Does NOT retry — reconnect supervisors layer their
/// own policy on top of this.
pub async fn spawn_once(
    addr: std::net::SocketAddr,
    identity: &proto::Identity,
    opts: HandshakeOptions,
) -> Result<(Arc<UdpConnection>, u16), Error> {
    let sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    sock.connect(addr).await?;
    // Force the driver to register the socket and observe writability:
    // otherwise the handshake's first try_send_to can hit a spurious
    // WouldBlock (readiness not cached yet) and the init packet is never
    // sent — the handshake then times out against a perfectly alive server.
    sock.writable().await?;
    let mut attempt_opts = opts.clone();
    // The hash-cash counter MUST match the identity sent in Init4, or
    // the server rejects the clientinit with 519.
    attempt_opts.client_key_offset = identity.counter();
    let conn = UdpConnection::spawn(sock, addr, attempt_opts, identity.key().clone()).await?;
    let clid = conn.clid;
    Ok((Arc::new(conn), clid))
}

/// Connect a client session over the native UDP protocol and return the
/// connection handle plus our assigned client id.
pub async fn connect(
    addr: std::net::SocketAddr,
    identity: &proto::Identity,
    opts: HandshakeOptions,
) -> Result<(Arc<UdpConnection>, u16), Error> {
    let mut last_err = None;
    for attempt in 0..3 {
        if attempt > 0 {
            // A failed attempt may leave a registered clone server-side;
            // give the server time to drop it before retrying.
            tokio::time::sleep(Duration::from_millis(2000)).await;
        }
        // A failed attempt can leave the identity registered as a clone;
        // retries use a fresh identity to avoid "too many clones" (521).
        let attempt_identity = if attempt == 0 {
            identity.clone()
        } else {
            Identity::create()
        };
        match spawn_once(addr, &attempt_identity, opts.clone()).await {
            Ok(pair) => return Ok(pair),
            Err(e) => {
                tracing::warn!(error = %e, "client handshake attempt failed, retrying");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(Error::Timeout))
}
