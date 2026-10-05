//! Integration tests for the client API additions: the connect options
//! extension (server password / privilege key at connect / uid pin),
//! streaming file transfer, channelmove, typed queries and the poke event.

use std::sync::Arc;
use std::time::Duration;

use test_support::Ts3Server;
use univox_core::event::Event;
use univox_core::id::ChannelId;
use univox_core::session::Session;
use univox_core::{ConnectOptions, Credential};
use univox_ts3::ext::Ts3Ext;
use univox_ts3::{Ts3ConnectOptions, Ts3Session};
use univox_ts3_proto::{Command, Identity, RowExt};

async fn spawn_server() -> Ts3Server {
    Ts3Server::start().await.expect("boot")
}

async fn connect_as(server: &Ts3Server, nickname: &str) -> Arc<Ts3Session> {
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous);
    Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect")
}

async fn spawn_admin(nickname: &str, server: &Ts3Server) -> Arc<Ts3Session> {
    let session = connect_as(server, nickname).await;
    session
        .use_privilege_key(&server.admin_token)
        .await
        .expect("token");
    session
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_upload_download_roundtrip() {
    let server = spawn_server().await;
    let session = spawn_admin("Streamer", &server).await;
    let channel = ChannelId::from_u64(1);
    let payload: Vec<u8> = (0..100_000u32).map(|i| (i % 253) as u8).collect();

    // Chunked upload with progress.
    let mut up = session
        .upload_file_stream(&channel, "/univox_stream.bin", payload.len() as u64, None)
        .await
        .expect("upload stream");
    assert_eq!(up.size(), payload.len() as u64);
    for chunk in payload.chunks(8192) {
        up.write_chunk(chunk).await.expect("write_chunk");
    }
    assert_eq!(up.written(), payload.len() as u64);
    up.finish().await.expect("finish");

    // Chunked download with progress.
    let mut dl = session
        .download_file_stream(&channel, "/univox_stream.bin", None)
        .await
        .expect("download stream");
    assert_eq!(dl.size(), payload.len() as u64);
    let mut got = Vec::with_capacity(payload.len());
    while let Some(chunk) = dl.next_chunk().await.expect("next_chunk") {
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, payload, "content mismatch");
    assert_eq!(dl.received(), payload.len() as u64);

    // Channel password on the transfer (plaintext, hashed internally):
    // root has none, so None must keep working after this test set a
    // password on another channel — use the same channel with an empty
    // password explicitly.
    session
        .delete_file(&channel, "/univox_stream.bin")
        .await
        .expect("cleanup");
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn streaming_upload_abort_discards_partial_file() {
    let server = spawn_server().await;
    let session = spawn_admin("Aborter", &server).await;
    let channel = ChannelId::from_u64(1);

    // Start an upload, write half, then drop the handle: the partial file
    // must not survive. The best-effort ftstop lands asynchronously —
    // poll for the disappearance.
    let mut up = session
        .upload_file_stream(&channel, "/univox_aborted.bin", 10_000, None)
        .await
        .expect("upload stream");
    up.write_chunk(&[0xAB; 4096]).await.expect("write_chunk");
    drop(up);

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut gone = false;
    let mut last_listing = Vec::new();
    while tokio::time::Instant::now() < deadline {
        let listing = session.list_files(&channel, "/").await.expect("list");
        if listing
            .iter()
            .all(|r| r.get("name").map(|n| n.trim_start_matches('/')) != Some("univox_aborted.bin"))
        {
            gone = true;
            break;
        }
        last_listing = listing;
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    assert!(gone, "partial upload survived the abort: {last_listing:?}");
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn move_channel_reparents_and_orders() {
    let server = spawn_server().await;
    let session = spawn_admin("Mover", &server).await;

    // Both channels live in the default channel (1): the move's `order`
    // sibling must be a child of the target parent.
    let a = session
        .create_channel(univox_core::model::ChannelOptions {
            name: "Moved One".into(),
            permanence: univox_core::model::Permanence::SemiPermanent,
            parent: Some(ChannelId::from_u64(1)),
            ..Default::default()
        })
        .await
        .expect("create a");
    let b = session
        .create_channel(univox_core::model::ChannelOptions {
            name: "Moved Two".into(),
            permanence: univox_core::model::Permanence::SemiPermanent,
            parent: Some(ChannelId::from_u64(1)),
            ..Default::default()
        })
        .await
        .expect("create b");

    // Move `a` below `b` within the default channel (1): channel_order
    // becomes b's id.
    session
        .move_channel(&a, &ChannelId::from_u64(1), b.as_u64().unwrap_or(0), None)
        .await
        .expect("move_channel");
    let rows = session
        .exec(Command::new("channellist"))
        .await
        .expect("channellist");
    let row = rows
        .iter()
        .find(|r| r.get("cid").and_then(|v| v.parse::<u64>().ok()) == a.as_u64())
        .expect("moved channel in list");
    assert_eq!(
        row.get("channel_order").and_then(|v| v.parse::<u64>().ok()),
        b.as_u64(),
        "channel_order should point at the sibling we sort after"
    );
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_queries_work() {
    let server = spawn_server().await;
    let session = spawn_admin("Querier", &server).await;

    let groups = session.server_groups().await.expect("server_groups");
    assert!(
        groups.iter().any(|g| g.name == "Server Admin"),
        "Server Admin group missing: {groups:?}"
    );
    assert!(
        groups.iter().any(|g| g.name == "Guest"),
        "Guest group missing: {groups:?}"
    );

    let cgroups = session.channel_groups().await.expect("channel_groups");
    assert!(
        cgroups.iter().any(|g| g.name == "Channel Admin"),
        "Channel Admin group missing: {cgroups:?}"
    );

    // A fresh client has no permlist entries; grant one directly on the
    // client's dbid (via admin query) so the typed query has data.
    let me = univox_core::id::MemberId::from_u64(univox_ts3::self_clid(&session));
    let uid = session.uid_from_clid(&me).await.expect("uid");
    let dbid = session
        .dbid_from_uid(&uid)
        .await
        .expect("dbid")
        .expect("dbid");
    let q = univox_ts3::QuerySession::connect(univox_ts3::QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some(server.serveradmin_password.clone()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query");
    q.exec(
        Command::new("clientaddperm")
            .param("cldbid", dbid.as_u64().unwrap_or(0))
            .param("permsid", "b_client_channel_textmessage_send")
            .param("permvalue", 1)
            .param("permskip", 0)
            .param("permnegated", 0),
    )
    .await
    .expect("clientaddperm");
    q.quit().await.ok();

    let perms = session.own_permissions().await.expect("own_permissions");
    assert!(
        perms
            .iter()
            .any(|(name, value)| name == "b_client_channel_textmessage_send" && *value == 1),
        "granted permission missing from own permlist: {perms:?}"
    );

    session.subscribe_all().await.expect("subscribe_all");
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_options_privilege_key_and_uid_pin() {
    let server = spawn_server().await;

    // Server's uid for the pin.
    let q = univox_ts3::QuerySession::connect(univox_ts3::QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some(server.serveradmin_password.clone()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query");
    let info = q.exec_one(Command::new("serverinfo")).await.expect("serverinfo");
    let uid = info.get("virtualserver_unique_identifier").expect("uid").to_string();

    // Correct pin + privilege key consumed at connect: the client ends up
    // in Server Admin (sgid 6) without a separate use_privilege_key call.
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("Tokened")
        .credential(Credential::Anonymous)
        .with_extension(Ts3ConnectOptions {
            privilege_key: Some(server.admin_token.clone()),
            server_uid_pin: Some(uid.clone()),
            ..Default::default()
        });
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect with pin + token");
    let rows = session
        .exec(Command::new("clientlist").opt("groups"))
        .await
        .expect("clientlist");
    let mine = rows
        .iter()
        .find(|r| r.get("clid") == Some(univox_ts3::self_clid(&session).to_string()).as_deref())
        .expect("own row");
    let groups = mine.get("client_servergroups").unwrap_or_default();
    assert!(
        groups.split(',').any(|g| g.trim() == "6"),
        "privilege key not applied at connect: servergroups={groups:?}"
    );
    session.disconnect(None).await.ok();

    // Wrong pin: the handshake must refuse (anti-DNS-hijack).
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("Pinned")
        .credential(Credential::Anonymous)
        .with_extension(Ts3ConnectOptions {
            server_uid_pin: Some("definitely/not/the/uid=".into()),
            ..Default::default()
        });
    let err = Ts3Session::connect(opts, Identity::create()).await;
    assert!(err.is_err(), "wrong uid pin must fail the handshake");

    q.quit().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_without_server_password_is_rejected() {
    let server = spawn_server().await;
    let q = univox_ts3::QuerySession::connect(univox_ts3::QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some(server.serveradmin_password.clone()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query");
    q.exec(Command::new("serveredit").param("virtualserver_password", "s3cret"))
        .await
        .expect("serveredit");

    // Without the password: refused at clientinit or kicked right after.
    // (A failed attempt also flood-bans the source IP, which is why this
    // runs on its own server instance.)
    let rejected = match Ts3Session::connect(
        ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
            .nickname("NoPassword")
            .credential(Credential::Anonymous),
        Identity::create(),
    )
    .await
    {
        Err(_) => true,
        Ok(s) => {
            tokio::time::sleep(Duration::from_secs(1)).await;
            s.state() != univox_core::session::SessionState::Connected
        }
    };
    assert!(rejected, "client without password should not stay connected");
    q.quit().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_with_server_password() {
    let server = spawn_server().await;

    // Password-protect the server via admin query. Empirically 3.13 takes
    // just the password (the flag follows implicitly).
    let q = univox_ts3::QuerySession::connect(univox_ts3::QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some(server.serveradmin_password.clone()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query");
    q.exec(Command::new("serveredit").param("virtualserver_password", "s3cret"))
        .await
        .expect("serveredit");

    // With the password (plaintext, hashed internally): accepted and stays.
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("WithPassword")
        .credential(Credential::Anonymous)
        .with_extension(Ts3ConnectOptions {
            server_password: Some("s3cret".into()),
            ..Default::default()
        });
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect with server password");
    // Still connected after a grace period (wrong-password clients are
    // kicked "invalid password" almost immediately).
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert_eq!(session.state(), univox_core::session::SessionState::Connected);
    session.disconnect(None).await.ok();
    q.quit().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn poke_delivers_message_created() {
    let server = spawn_server().await;
    // The admin token is a single-use privilege key — only Alice gets it;
    // Bob stays a guest (guests may poke).
    let a = spawn_admin("Alice", &server).await;
    let b = connect_as(&server, "Bob").await;

    let mut events = a.events();
    let a_clid = univox_ts3::self_clid(&a);
    let b_clid = univox_ts3::self_clid(&b);
    b.poke(&univox_core::id::MemberId::from_u64(a_clid), "knock knock")
        .await
        .expect("poke");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut hit = None;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), events.next()).await {
            Ok(Some(ev)) => {
                if let Event::MessageCreated { message } = &*ev {
                    hit = Some(message.clone());
                    break;
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    let message = hit.expect("no MessageCreated for the poke");
    assert_eq!(message.content, "knock knock");
    assert_eq!(message.author_name, "Bob");
    // The agreed mapping: the poke records its invoker as the target so
    // consumers can reply in kind.
    assert_eq!(
        message.target,
        Some(univox_core::model::MessageTarget::Poke(
            univox_core::id::MemberId::from_u64(b_clid)
        ))
    );
    a.disconnect(None).await.ok();
    b.disconnect(None).await.ok();
}

