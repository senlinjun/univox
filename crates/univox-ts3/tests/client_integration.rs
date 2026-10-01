//! Client protocol integration tests: connect to the local TS3 server over
//! the native UDP protocol. Note that a Guest-level client does not have
//! `b_virtualserver_channel_list`; the server *pushes* the channel list and
//! notifications after login, which is what the bookkeeping consumes.

use std::time::Duration;

use test_support::Ts3Server;
use univox_ts3::client::{self, HandshakeOptions};
use univox_ts3_proto::{Command, Identity, RowExt};

fn udp_socket() -> std::net::UdpSocket {
    // Bind a nonblocking socket so tokio can register it with the reactor.
    let sock = std::net::UdpSocket::bind("0.0.0.0:0").expect("bind local udp");
    sock.set_nonblocking(true).expect("nonblocking");
    sock
}

#[tokio::test(flavor = "multi_thread")]
async fn client_connects_runs_commands_and_receives_pushes() {
    let server = Ts3Server::start().await.expect("boot");
    let identity = Identity::create();

    let addr = format!("127.0.0.1:{}", server.voice_port);
    let opts = HandshakeOptions {
        nickname: "Univox Test Bot".into(),
        client_key_offset: identity.counter(),
        ..Default::default()
    };
    let (conn, _clid) = client::connect(udp_socket(), addr.parse().unwrap(), &identity, opts)
        .await
        .expect("client connect");

    // Subscribe to notifications before issuing commands.
    let mut notifications = conn.subscribe();

    // whoami: the response rows have no command name; completion comes via
    // the echoed return_code in the error packet.
    let rows = conn.exec(Command::new("whoami")).await.expect("whoami");
    assert!(!rows.is_empty());
    let me = rows.into_iter().next().unwrap();
    assert_eq!(me.get("client_nickname"), Some("Univox Test Bot"));

    // The server pushed the channel list at login.
    let mut saw_channellist = false;
    let mut saw_enterview = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && !(saw_channellist && saw_enterview) {
        match tokio::time::timeout(Duration::from_millis(500), notifications.recv()).await {
            Ok(Ok(cmd)) => match cmd.name.as_str() {
                "channellist" => saw_channellist = true,
                "notifycliententerview" => saw_enterview = true,
                _ => {}
            },
            _ => break,
        }
    }
    assert!(saw_channellist, "no pushed channellist");
    assert!(saw_enterview, "no cliententerview notification");

    // Send a channel text message (Guests may speak in the default channel).
    conn.exec(
        Command::new("sendtextmessage")
            .param("targetmode", 2)
            .param("msg", "hello from univox"),
    )
    .await
    .expect("sendtextmessage");

    conn.disconnect(1, "test done").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn client_two_clients_see_each_other() {
    let server = Ts3Server::start().await.expect("boot");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", server.voice_port).parse().unwrap();

    let identity_a = Identity::create();
    let opts_a = HandshakeOptions {
        nickname: "Bot A".into(),
        client_key_offset: identity_a.counter(),
        ..Default::default()
    };
    let (conn_a, _) = client::connect(udp_socket(), addr, &identity_a, opts_a)
        .await
        .expect("connect A");

    // Bot A listens for other clients entering.
    let mut notifs = conn_a.subscribe();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let identity_b = Identity::create();
    let opts_b = HandshakeOptions {
        nickname: "Bot B".into(),
        client_key_offset: identity_b.counter(),
        ..Default::default()
    };
    let (conn_b, _) = client::connect(udp_socket(), addr, &identity_b, opts_b)
        .await
        .expect("connect B");

    let _ = conn_b.exec(Command::new("whoami")).await;

    // A should be notified that B entered.
    let mut saw_b = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !saw_b {
        match tokio::time::timeout(Duration::from_millis(500), notifs.recv()).await {
            Ok(Ok(cmd)) if cmd.name == "notifycliententerview" => {
                if cmd.get("client_nickname") == Some("Bot B") {
                    saw_b = true;
                }
            }
            _ => {}
        }
    }
    assert!(saw_b, "Bot A did not see Bot B join");

    conn_a.disconnect(1, "bye").await;
    conn_b.disconnect(1, "bye").await;
}
