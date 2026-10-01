//! Native TS3 client protocol (UDP voice + control).

pub mod connection;

pub use connection::{HandshakeOptions, UdpConnection};

use std::sync::Arc;

use univox_ts3_proto as proto;
use univox_ts3_proto::{Command, Error};

/// Connect a client session over the native UDP protocol and return the
/// connection handle plus our assigned client id.
pub async fn connect(
    sock: std::net::UdpSocket,
    addr: std::net::SocketAddr,
    identity: &proto::Identity,
    opts: HandshakeOptions,
) -> Result<(Arc<UdpConnection>, u16), Error> {
    let sock = tokio::net::UdpSocket::from_std(sock)?;
    sock.connect(addr).await?;
    let conn = UdpConnection::spawn(
        sock,
        addr,
        opts,
        identity.key().clone(),
    )
    .await?;
    Ok((Arc::new(conn), 0))
}
