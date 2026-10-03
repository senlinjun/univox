//! Lifecycle integration tests: reconnect on server loss, attempt
//! exhaustion, user disconnect, connection stats (FEATURES.md §2.2/§2.4/§2.5).

use std::sync::Arc;
use std::time::Duration;

use test_support::{Ts3Server, Ts3ServerOptions};
use univox_core::connect::ReconnectPolicy;
use univox_core::event::Event;
use univox_core::session::{Session, SessionState};
use univox_core::{ConnectOptions, Credential};
use univox_ts3::Ts3Session;
use univox_ts3_proto::Identity;

fn fast_policy(max_attempts: u32) -> ReconnectPolicy {
    ReconnectPolicy {
        max_attempts,
        base_delay: Duration::from_millis(200),
        max_delay: Duration::from_secs(2),
        jitter: 0.0,
        restore_state: true,
    }
}

async fn connect_session_with(
    server: &Ts3Server,
    nickname: &str,
    policy: ReconnectPolicy,
) -> Arc<Ts3Session> {
    let mut opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous);
    opts.reconnect = policy;
    Ts3Session::connect(opts, Identity::create())
        .await
        .expect("session connect")
}

async fn connect_session(server: &Ts3Server, nickname: &str) -> Arc<Ts3Session> {
    connect_session_with(
        server,
        nickname,
        ReconnectPolicy {
            // No reconnects for the plain tests.
            max_attempts: 0,
            ..fast_policy(0)
        },
    )
    .await
}

/// Server restart on the same voice port: the session detects the loss,
/// reconnects once the new server is up, and re-fills the book.
#[tokio::test(flavor = "multi_thread")]
async fn reconnects_after_server_restart() {
    // Pick a fixed voice port both server instances will use.
    let voice_port = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let server_a = Ts3Server::start_with(Ts3ServerOptions {
        voice_port: Some(voice_port),
        ..Default::default()
    })
    .await
    .expect("boot server A");
    let session = connect_session_with(&server_a, "Phoenix", fast_policy(10)).await;
    let mut events = session.events();
    assert_eq!(session.state(), SessionState::Connected);

    // Server A dies; a fresh server B takes over the same endpoint.
    drop(server_a);
    let server_b = Ts3Server::start_with(Ts3ServerOptions {
        voice_port: Some(voice_port),
        ..Default::default()
    })
    .await
    .expect("boot server B");
    let _keep = server_b;

    // Give the session a chance to notice the drop while B boots.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let mut saw = (false, false, false); // temporarily-disconnected, reconnecting, reconnected
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while tokio::time::Instant::now() < deadline && saw.2 == false {
        match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
            Ok(Some(ev)) => match ev.as_ref() {
                Event::TemporarilyDisconnected { .. } => saw.0 = true,
                Event::Reconnecting => saw.1 = true,
                Event::Reconnected => saw.2 = true,
                _ => {}
            },
            _ => {}
        }
    }
    assert!(saw.0, "no TemporarilyDisconnected event");
    assert!(saw.1, "no Reconnecting event");
    assert!(saw.2, "no Reconnected event within 40s");
    assert_eq!(session.state(), SessionState::Connected);
    assert!(session.stats().reconnect_count >= 1, "reconnect_count not updated");

    // The book mirror is re-primed against the new server.
    let mut channels = 0;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        channels = session.book().channels().len();
        if channels >= 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(channels >= 1, "book not re-populated after reconnect");

    session.disconnect(Some("done".into())).await.ok();
}

/// No server to return to: attempts are exhausted and the session closes.
#[tokio::test(flavor = "multi_thread")]
async fn exhausts_attempts_and_closes_when_server_gone() {
    let server = Ts3Server::start().await.expect("boot");
    let session = connect_session_with(&server, "Stranded", fast_policy(3)).await;
    let mut events = session.events();
    assert_eq!(session.state(), SessionState::Connected);

    // The server dies with no replacement.
    drop(server);

    let mut closed_reason = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
            Ok(Some(ev)) => {
                if let Event::Closed { reason } = ev.as_ref() {
                    closed_reason = Some(reason.clone());
                    break;
                }
            }
            _ => {}
        }
    }
    let reason = closed_reason.expect("no Closed event after exhausting reconnects");
    assert!(
        matches!(reason, univox_core::model::DisconnectReason::Network(_)),
        "expected Network close, got {reason:?}"
    );
    assert_eq!(session.state(), SessionState::Disconnected);
}

/// User-requested disconnect: no reconnect attempts afterwards.
#[tokio::test(flavor = "multi_thread")]
async fn user_disconnect_does_not_reconnect() {
    let server = Ts3Server::start().await.expect("boot");
    let session = connect_session(&server, "Quitter").await;
    let mut events = session.events();

    session.disconnect(Some("bye".into())).await.expect("disconnect");
    assert_eq!(session.state(), SessionState::Disconnected);

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert_eq!(session.state(), SessionState::Disconnected);

    let mut saw_closed = false;
    let mut saw_reconnecting = false;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(100), events.next()).await
    {
        match ev.as_ref() {
            Event::Closed { reason } => {
                assert!(
                    matches!(reason, univox_core::model::DisconnectReason::Requested { .. }),
                    "expected Requested close, got {reason:?}"
                );
                saw_closed = true;
            }
            Event::Reconnecting | Event::Reconnected => saw_reconnecting = true,
            _ => {}
        }
    }
    assert!(saw_closed, "no Closed event after user disconnect");
    assert!(!saw_reconnecting, "session tried to reconnect after user disconnect");
}

/// The actor collects traffic counters and ping RTT (FEATURES.md §2.5).
#[tokio::test(flavor = "multi_thread")]
async fn connection_stats_are_populated() {
    let server = Ts3Server::start().await.expect("boot");
    let session = connect_session(&server, "Meter").await;

    // Pings go out every second; wait for at least one round trip.
    tokio::time::sleep(Duration::from_millis(1800)).await;

    let stats = session.stats();
    assert!(stats.ping.is_some(), "no ping RTT measured");
    assert!(stats.bandwidth_down > 0, "no bytes received");
    assert!(stats.bandwidth_up > 0, "no bytes sent");

    session.disconnect(None).await.ok();
}
