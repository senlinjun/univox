//! Native TS3 client protocol (UDP voice + control).

pub mod connection;

pub use connection::{HandshakeOptions, Rows, UdpConnection};

use std::sync::Arc;
use std::time::Duration;

use univox_ts3_proto as proto;
use univox_ts3_proto::Identity;
use univox_ts3_proto::{Command, Error};

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
        // A fresh local socket per attempt: the server keys connections by
        // source address, so reusing the socket would continue the previous
        // (broken) server-side connection state instead of starting over.
        let attempt_sock = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
        attempt_sock.connect(addr).await?;
        // A failed attempt can leave the identity registered as a clone;
        // retries use a fresh identity to avoid "too many clones" (521).
        let attempt_identity = if attempt == 0 {
            identity.clone()
        } else {
            Identity::create()
        };
        // The hash-cash counter MUST match the identity sent in Init4, or
        // the server rejects the clientinit with 519.
        let mut attempt_opts = opts.clone();
        attempt_opts.client_key_offset = attempt_identity.counter();
        match UdpConnection::spawn(
            attempt_sock,
            addr,
            attempt_opts,
            attempt_identity.key().clone(),
        )
        .await
        {
            Ok(conn) => {
                let clid = conn.clid;
                return Ok((Arc::new(conn), clid));
            }
            Err(e) => {
                tracing::warn!(error = %e, "client handshake attempt failed, retrying");
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or(Error::Timeout))
}
