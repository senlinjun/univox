//! Client protocol integration tests: connect to the local TS3 server over
//! the native UDP protocol. Note that a Guest-level client does not have
//! `b_virtualserver_channel_list`; the server *pushes* the channel list and
//! notifications after login, which is what the bookkeeping consumes.

use std::time::Duration;

use test_support::Ts3Server;

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_test_writer()
        .try_init();
}
use univox_ts3::client::{self, HandshakeOptions};
use univox_ts3_proto::{Command, Identity, RowExt};

#[tokio::test(flavor = "multi_thread")]
async fn client_connects_runs_commands_and_receives_pushes() {
    init_tracing();
    let server = Ts3Server::start().await.expect("boot");
    let identity = Identity::create();

    let addr = format!("127.0.0.1:{}", server.voice_port);
    let opts = HandshakeOptions {
        nickname: "Univox Test Bot".into(),
        client_key_offset: identity.counter(),
        ..Default::default()
    };
    let (conn, _clid) = match client::connect(addr.parse().unwrap(), &identity, opts).await {
        Ok(c) => c,
        Err(e) => {
            for line in server.output_lines() {
                eprintln!("SRV: {line}");
            }
            panic!("client connect failed: {e}");
        }
    };

    // Subscribe to notifications before issuing commands. The server pushes
    // the login dumps (channellist, cliententerview, ...) right after
    // clientinit; the replay buffer makes sure we still see them.
    let mut notifications = conn.subscribe();

    // whoami: completes via the echoed return_code; the response rows flow
    // through the notification channel.
    conn.exec(Command::new("whoami")).await.expect("whoami");

    let mut saw_channellist = false;
    let mut saw_enterview = false;
    let mut saw_whoami = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline && !(saw_channellist && saw_enterview && saw_whoami) {
        match tokio::time::timeout(Duration::from_millis(500), notifications.recv()).await {
            Ok(Some(cmd)) => {
                // Empty-valued fields arrive as bare keys, so the parsed
                // command "name" varies with the server's field order —
                // match the whoami rows by content instead.
                match cmd.name.as_str() {
                    "channellist" => saw_channellist = true,
                    "notifycliententerview" => saw_enterview = true,
                    _ => {}
                }
                if cmd.get("client_nickname") == Some("Univox Test Bot") {
                    saw_whoami = true;
                }
            }
            _ => break,
        }
    }
    // Some server runs skip the pushed channellist; request it explicitly.
    // Guests cannot read the channel list, so take the admin token first —
    // the exec result then carries the rows deterministically.
    if !saw_channellist {
        let _ = conn
            .exec(Command::new("privilegekeyuse").param("token", &server.admin_token))
            .await;
        match conn.exec(Command::new("channellist").opt("topic")).await {
            Ok(rows) if !rows.is_empty() => saw_channellist = true,
            _ => {}
        }
    }
    assert!(saw_channellist, "no channellist (pushed or requested)");
    assert!(saw_enterview, "no cliententerview notification");
    assert!(saw_whoami, "whoami response rows missing");

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
    init_tracing();
    let server = Ts3Server::start().await.expect("boot");
    let addr: std::net::SocketAddr = format!("127.0.0.1:{}", server.voice_port).parse().unwrap();

    let identity_a = Identity::create();
    let opts_a = HandshakeOptions {
        nickname: "Bot A".into(),
        client_key_offset: identity_a.counter(),
        ..Default::default()
    };
    let (conn_a, _) = client::connect(addr, &identity_a, opts_a)
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
    let (conn_b, _) = client::connect(addr, &identity_b, opts_b)
        .await
        .expect("connect B");

    let _ = conn_b.exec(Command::new("whoami")).await;

    // A should be notified that B entered.
    let mut saw_b = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !saw_b {
        match tokio::time::timeout(Duration::from_millis(500), notifs.recv()).await {
            Ok(Some(cmd)) if cmd.name == "notifycliententerview" => {
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

