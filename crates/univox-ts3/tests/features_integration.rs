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
        .delete_file(&channel, "/univox_stream.bin", None)
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
        let listing = session.list_files(&channel, "/", None).await.expect("list");
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


#[tokio::test(flavor = "multi_thread")]
async fn identity_counter_survives_reconnect() {
    // The hash-cash counter is a proof-of-work stamp, not a consumed
    // nonce: the same identity/counter connects twice in a row (matches
    // tsclientlib, which never increments per attempt — lib.rs just sends
    // identity.counter() as client_key_offset).
    let server = spawn_server().await;
    let addr = format!("127.0.0.1:{}", server.voice_port);
    let identity = Identity::create();

    let s1 = Ts3Session::connect(
        ConnectOptions::new(addr.clone()).nickname("Reuser").credential(Credential::Anonymous),
        identity.clone(),
    )
    .await
    .expect("first connect");
    assert_eq!(
        s1.identity().counter(),
        identity.counter(),
        "connect itself must not move the counter"
    );
    s1.disconnect(None).await.ok();
    // The server releases the old client record asynchronously; a clone
    // connecting too fast is rejected.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let s2 = Ts3Session::connect(
        ConnectOptions::new(addr).nickname("Reuser").credential(Credential::Anonymous),
        identity.clone(),
    )
    .await
    .expect("second connect with the same identity/counter");
    assert_eq!(s2.identity().counter(), identity.counter());
    s2.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn raised_security_level_requires_upgrade() {
    let server = spawn_server().await;
    let addr = format!("127.0.0.1:{}", server.voice_port);
    let q = univox_ts3::QuerySession::connect(univox_ts3::QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some(server.serveradmin_password.clone()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query");
    q.exec(Command::new("serveredit").param("virtualserver_needed_identity_security_level", 14))
        .await
        .expect("raise required level");

    // With upgrade_identity_to the identity is raised before clientinit
    // and the session exposes the final state for persistence.
    let upgraded = Ts3Session::connect(
        ConnectOptions::new(addr.clone())
            .nickname("Upgraded")
            .credential(Credential::Anonymous)
            .with_extension(Ts3ConnectOptions {
                upgrade_identity_to: Some(14),
                ..Default::default()
            }),
        Identity::create(),
    )
    .await
    .expect("connect with upgrade_identity_to");
    assert!(upgraded.identity().level() >= 14, "readback level");
    upgraded.disconnect(None).await.ok();

    // A default level-8 identity is refused (clientinit error, or dropped
    // right after). Runs last: a failed attempt may flood-ban the source.
    let plain = Ts3Session::connect(
        ConnectOptions::new(addr).nickname("LowLevel").credential(Credential::Anonymous),
        Identity::create(),
    )
    .await;
    let rejected = match plain {
        Err(_) => true,
        Ok(s) => {
            tokio::time::sleep(Duration::from_millis(800)).await;
            s.state() != univox_core::session::SessionState::Connected
        }
    };
    assert!(rejected, "level-8 identity must not stay on a level-14 server");
    q.quit().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn create_dir_shows_in_listing() {
    let server = spawn_server().await;
    let session = spawn_admin("DirMaker", &server).await;
    let channel = ChannelId::from_u64(1);

    session.create_dir(&channel, "/univox_dir", None).await.expect("ftcreatedir");
    let listing = session.list_files(&channel, "/", None).await.expect("list");
    let entry = listing
        .iter()
        .find(|r| r.get("name").map(|n| n.trim_start_matches('/')) == Some("univox_dir"))
        .expect("directory missing from listing");
    assert_eq!(entry.get("type"), Some("0"), "directories have type 0");

    // Empty directories delete like files.
    session.delete_file(&channel, "/univox_dir", None).await.expect("cleanup");
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn client_moved_carries_kick_reason() {
    // Live-captured on 3.13.8: the moved client receives
    // `notifyclientmoved reasonid=1` for a move and `reasonid=4` +
    // `reasonmsg` + invoker for a channel kick — both mapped onto
    // Event::ClientMoved with a structured reason.
    use univox_core::id::MemberId;
    use univox_core::model::{ChannelOptions, ClientMoveReason, Permanence};
    let server = spawn_server().await;
    let a = spawn_admin("Admin", &server).await;
    let b = connect_as(&server, "Victim").await;
    let a_id = MemberId::from_u64(univox_ts3::self_clid(&a));
    let mut events = b.events();

    let ch = a
        .create_channel(ChannelOptions {
            name: "KickMe".into(),
            parent: Some(ChannelId::from_u64(1)),
            permanence: Permanence::SemiPermanent,
            ..Default::default()
        })
        .await
        .expect("create");
    a.move_member(&MemberId::from_u64(univox_ts3::self_clid(&b)), &ch)
        .await
        .expect("move victim in");
    tokio::time::sleep(Duration::from_millis(500)).await;
    a.kick_member(
        &MemberId::from_u64(univox_ts3::self_clid(&b)),
        true,
        Some("out you go"),
    )
    .await
    .expect("kick");

    let mut moved = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(300), events.next()).await {
            Ok(Some(ev)) => {
                if let Event::ClientMoved { reason, .. } = &*ev {
                    moved.push(reason.clone());
                }
            }
            Ok(None) | Err(_) => break,
        }
    }
    assert!(
        moved.iter().any(|r| matches!(r, ClientMoveReason::Moved)),
        "move reason missing: {moved:?}"
    );
    assert!(
        moved.iter().any(|r| matches!(
            r,
            ClientMoveReason::ChannelKicked { by, message }
                if *by == Some(a_id.clone()) && message == "out you go"
        )),
        "kick reason missing: {moved:?}"
    );
    a.disconnect(None).await.ok();
    b.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn reconnect_restores_self_state() {
    use test_support::{Ts3Server, Ts3ServerOptions};
    use univox_ts3::ext::Ts3Ext as _;

    let voice_port = {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        probe.local_addr().unwrap().port()
    };
    let server_a = Ts3Server::start_with(Ts3ServerOptions {
        voice_port: Some(voice_port),
        ..Default::default()
    })
    .await
    .expect("boot A");
    let addr = format!("127.0.0.1:{voice_port}");
    // A patient policy: under full-suite load server B boots slower than
    // the default attempt budget.
    let policy = univox_core::connect::ReconnectPolicy {
        max_attempts: 20,
        base_delay: Duration::from_millis(200),
        max_delay: Duration::from_secs(2),
        jitter: 0.0,
        restore_state: true,
    };
    let mut opts = ConnectOptions::new(addr.clone())
        .nickname("Phoenix")
        .credential(Credential::Anonymous);
    opts.reconnect = policy;
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect");

    session
        .update_self(univox_ts3::SelfUpdate {
            away: Some(true),
            away_message: Some("brb".into()),
            ..Default::default()
        })
        .await
        .expect("away");

    let mut events = session.events();
    drop(server_a);
    let server_b = Ts3Server::start_with(Ts3ServerOptions {
        voice_port: Some(voice_port),
        ..Default::default()
    })
    .await
    .expect("boot B");
    let _keep = server_b;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(40);
    let mut reconnected = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), events.next()).await {
            Ok(Some(ev)) if matches!(&*ev, Event::Reconnected) => {
                reconnected = true;
                break;
            }
            Ok(Some(_)) | Ok(None) | Err(_) => {}
        }
    }
    assert!(reconnected, "no Reconnected event");

    // The replayed clientupdate echoes back as notifyclientupdated, which
    // the book mirror records (works for guests — no clientlist needed).
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut restored = None;
    while tokio::time::Instant::now() < deadline {
        let me = univox_core::id::MemberId::from_u64(univox_ts3::self_clid(&session));
        if let Some(st) = session.book().member_state(&me) {
            if st.away {
                restored = Some(st);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let st = restored.expect("away state not restored after reconnect");
    assert_eq!(st.away_message.as_deref(), Some("brb"));
    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn passworded_channel_transfer_and_join() {
    use univox_core::model::{ChannelOptions, Permanence};
    let server = spawn_server().await;
    let session = spawn_admin("PwOps", &server).await;

    let ch = session
        .create_channel(ChannelOptions {
            name: "Vault".into(),
            parent: Some(ChannelId::from_u64(1)),
            permanence: Permanence::SemiPermanent,
            password: Some("chpw".into()),
            ..Default::default()
        })
        .await
        .expect("create passworded channel");

    // Upload + list + download + delete with the channel password
    // (plaintext, hashed internally).
    let payload = b"secret payload".to_vec();
    let mut up = session
        .upload_file_stream(&ch, "/secret.txt", payload.len() as u64, Some("chpw"))
        .await
        .expect("upload stream");
    up.write_chunk(&payload).await.expect("write");
    up.finish().await.expect("finish");

    let listing = session
        .list_files(&ch, "/", Some("chpw"))
        .await
        .expect("list with password");
    assert!(
        listing
            .iter()
            .any(|r| r.get("name").map(|n| n.trim_start_matches('/')) == Some("secret.txt")),
        "file missing: {listing:?}"
    );

    let mut dl = session
        .download_file_stream(&ch, "/secret.txt", Some("chpw"))
        .await
        .expect("download stream");
    let mut got = Vec::new();
    while let Some(chunk) = dl.next_chunk().await.expect("chunk") {
        got.extend_from_slice(&chunk);
    }
    assert_eq!(got, payload);

    session
        .delete_file(&ch, "/secret.txt", Some("chpw"))
        .await
        .expect("delete with password");
    let listing = session
        .list_files(&ch, "/", Some("chpw"))
        .await
        .expect("list after delete");
    assert!(
        listing
            .iter()
            .all(|r| r.get("name").map(|n| n.trim_start_matches('/')) != Some("secret.txt")),
        "file not deleted: {listing:?}"
    );

    // join_voice with the channel password: cpw rides clientmove.
    session
        .join_voice(&ch, Some("chpw"))
        .await
        .expect("join with password");
    let me = univox_ts3::self_clid(&session);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    let mut joined = false;
    while tokio::time::Instant::now() < deadline && !joined {
        joined = session.book().members().iter().any(|m| {
            m.id.as_u64() == Some(me)
                && m.channel_id.as_ref().and_then(|c| c.as_u64()) == ch.as_u64()
        });
        if !joined {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    assert!(joined, "join_voice with password did not move us into the channel");

    // Wrong password: refused.
    assert!(
        session.join_voice(&ch, Some("wrong")).await.is_err(),
        "wrong channel password must be refused"
    );
    session.disconnect(None).await.ok();
}

// ---- temporary passwords (§8.2) ----

#[tokio::test(flavor = "multi_thread")]
async fn temp_password_roundtrip() {
    let server = spawn_server().await;
    let session = spawn_admin("Temp Pw", &server).await;
    let channel = ChannelId::from_u64(1);

    assert!(session.temp_passwords().await.expect("list").is_empty());
    session
        .add_temp_password(&channel, "temp-pass-1", "univox test", Some(Duration::from_secs(60)), None)
        .await
        .expect("add");
    let list = session.temp_passwords().await.expect("list");
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].password, "temp-pass-1");
    assert_eq!(list[0].description, "univox test");
    assert_eq!(list[0].channel, channel);

    session.remove_temp_password("temp-pass-1").await.expect("del");
    assert!(session.temp_passwords().await.expect("list").is_empty());

    session.disconnect(None).await.ok();
}

// ---- channel group assignment (§9.3) ----

#[tokio::test(flavor = "multi_thread")]
async fn channel_group_assignment_roundtrip() {
    let server = spawn_server().await;
    let admin = spawn_admin("Cg Admin", &server).await;
    let member = connect_as(&server, "Cg Member").await;

    // A regular channel group (skip templates and query groups).
    let groups = admin.channel_groups().await.expect("channel_groups");
    let group = groups.iter().find(|g| g.kind == 1).expect("regular channel group");

    let member_id = member
        .book()
        .with(|b| b.self_member.member_id.clone())
        .flatten()
        .expect("member clid");
    admin
        .set_member_channel_group(&member_id, &ChannelId::from_u64(1), group.id)
        .await
        .expect("setclientchannelgroup");

    let assignments = admin
        .channel_group_members(Some(&ChannelId::from_u64(1)))
        .await
        .expect("channelgroupclientlist");
    let mine = assignments
        .iter()
        .find(|a| a.group == group.id)
        .expect("assignment recorded");
    assert_eq!(mine.channel, ChannelId::from_u64(1));

    member.disconnect(None).await.ok();
    admin.disconnect(None).await.ok();
}

// ---- local password verification (§11) ----

#[tokio::test(flavor = "multi_thread")]
async fn verify_channel_password_positive_and_negative() {
    let server = spawn_server().await;
    let session = spawn_admin("Verify Bot", &server).await;

    // The default channel has no password: everything "verifies".
    let default_channel = ChannelId::from_u64(1);
    assert!(session
        .verify_channel_password(&default_channel, "")
        .await
        .expect("verify"));

    let channel = session
        .create_channel(univox_core::model::ChannelOptions {
            name: "Verify Vault".into(),
            parent: Some(ChannelId::from_u64(1)),
            permanence: univox_core::model::Permanence::SemiPermanent,
            password: Some("sekret".into()),
            ..Default::default()
        })
        .await
        .expect("create passworded channel");

    assert!(session.verify_channel_password(&channel, "sekret").await.expect("verify"));
    assert!(!session.verify_channel_password(&channel, "wrong").await.expect("verify"));

    session.disconnect(None).await.ok();
}

// ---- talk power (§9.5) ----

#[tokio::test(flavor = "multi_thread")]
async fn talk_power_request_is_accepted_and_cancellable() {
    let server = spawn_server().await;
    let requester = spawn_admin("TP Requester", &server).await;

    // 3.13.8 accepts the client_talk_request_time spelling (see
    // request_talk_power docs); whether/how it relays the request is
    // server-dependent, so only the API round-trip is asserted here.
    requester.request_talk_power(Some("may I speak?")).await.expect("request");
    requester.cancel_talk_power_request().await.expect("cancel");

    requester.disconnect(None).await.ok();
}


/// Does `dbid` currently carry server group `sgid`?
async fn member_has_group(
    admin: &Arc<Ts3Session>,
    dbid: &univox_core::id::DbId,
    sgid: u64,
) -> bool {
    let rows = admin
        .exec(
            Command::new("servergroupsbyclientid")
                .param("cldbid", dbid.as_u64().unwrap_or(0)),
        )
        .await
        .expect("servergroupsbyclientid");
    rows.iter()
        .any(|r| r.get("sgid").and_then(|v| v.parse::<u64>().ok()) == Some(sgid))
}

#[tokio::test(flavor = "multi_thread")]
async fn talk_power_grant_is_auto_revoked() {
    let server = spawn_server().await;
    let admin = spawn_admin("TP Admin", &server).await;
    let member = connect_as(&server, "TP Member").await;

    // A regular server group the admin may assign (admin token = Server Admin).
    let groups = admin.server_groups().await.expect("server_groups");
    let group = groups.iter().find(|g| g.kind == 1).expect("regular server group");

    let member_id = member
        .book()
        .with(|b| b.self_member.member_id.clone())
        .flatten()
        .expect("member clid");
    let uid = admin.uid_from_clid(&member_id).await.expect("uid");
    let dbid = admin.dbid_from_uid(&uid).await.expect("dbid").expect("dbid");

    admin
        .grant_talk_power(&member_id, group.id.as_u64().unwrap_or(0), Some(Duration::from_millis(800)))
        .await
        .expect("grant");

    assert!(
        member_has_group(&admin, &dbid, group.id.as_u64().unwrap_or(0)).await,
        "group assigned"
    );

    // The auto-revoke fires after the duration.
    tokio::time::sleep(Duration::from_millis(2000)).await;
    assert!(
        !member_has_group(&admin, &dbid, group.id.as_u64().unwrap_or(0)).await,
        "group revoked after duration"
    );

    member.disconnect(None).await.ok();
    admin.disconnect(None).await.ok();
}

// ---- connection / local info queries (§11) ----

#[tokio::test(flavor = "multi_thread")]
async fn connection_info_queries() {
    let server = spawn_server().await;
    // `clientlist -times` needs elevated permissions.
    let session = spawn_admin("Info Bot", &server).await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let me = session
        .book()
        .with(|b| b.self_member.member_id.clone())
        .flatten()
        .expect("self clid");

    let idle = session.member_idle_time(&me).await.expect("idle time");
    assert!(idle < Duration::from_secs(60), "fresh session barely idle: {idle:?}");

    let info = session.member_connection_info(&me).await.expect("member info");
    assert!(info.idle_time.is_some(), "idle present: {info:?}");

    let server_info = session.server_connection_info().await.expect("server info");
    assert!(
        server_info.upstream_packetloss_total.is_some(),
        "server-side loss measurement present: {server_info:?}"
    );

    session.disconnect(None).await.ok();
}

// ---- icons & banner (§8.1/§11) ----

#[tokio::test(flavor = "multi_thread")]
async fn icon_upload_download_and_assignment() {
    let server = spawn_server().await;
    let session = spawn_admin("Icon Bot", &server).await;
    let payload: Vec<u8> = (0..4096u32).map(|i| (i % 7) as u8).collect();

    let id = session.upload_icon(&payload).await.expect("upload_icon");
    let got = session.download_icon(id).await.expect("download_icon");
    assert_eq!(got, payload, "icon roundtrip");

    // Same content ⇒ same id (content-addressed).
    let id2 = session.upload_icon(&payload).await.expect("re-upload");
    assert_eq!(id, id2);

    session.set_channel_icon(&ChannelId::from_u64(1), id).await.expect("set_channel_icon");
    let icons = session
        .exec(Command::new("channellist").opt("icon"))
        .await
        .expect("channellist");
    let cid_row = icons
        .iter()
        .find(|r| r.get("cid") == Some("1"))
        .expect("channel 1 row");
    assert_eq!(cid_row.get("channel_icon_id"), Some(id.to_string().as_str()));

    let member_icon = session.set_member_icon(&payload).await.expect("set_member_icon");
    assert_eq!(member_icon, id);
    // Client icons are stored as the i_icon_id permission on our dbid.
    let perms = session.own_permissions().await.expect("own_permissions");
    assert!(
        perms
            .iter()
            .any(|(name, value)| name == "i_icon_id" && *value as u32 as i64 == id),
        "i_icon_id permission missing: {perms:?}"
    );

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn host_banner_edit() {
    let server = spawn_server().await;
    let session = spawn_admin("Banner Bot", &server).await;

    session
        .set_host_banner("https://univox.test", Some("https://univox.test/banner.png"), Some(60), Some(1))
        .await
        .expect("set_host_banner");

    // Read back through a fresh connection's initserver dump.
    let reader = connect_as(&server, "Banner Reader").await;
    let extra_has = |key: &str, want: &str| {
        let key = key.to_owned();
        let want = want.to_owned();
        let reader = reader.clone();
        async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            reader
                .book()
                .with(|b| {
                    b.server
                        .as_ref()
                        .and_then(|s| s.extra.get(&key).cloned())
                })
                .flatten()
                == Some(want)
        }
    };
    assert!(extra_has("virtualserver_hostbanner_url", "https://univox.test").await);
    assert!(extra_has("virtualserver_hostbanner_gfx_url", "https://univox.test/banner.png").await);
    assert!(extra_has("virtualserver_hostbanner_mode", "1").await);

    session.disconnect(None).await.ok();
    reader.disconnect(None).await.ok();
}
