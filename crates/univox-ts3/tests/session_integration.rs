//! Unified Ts3Session integration tests: book mirror, events, messaging.

use std::time::Duration;

use test_support::Ts3Server;
use univox_core::event::Event;
use univox_core::model::{MessageTarget, Permanence};
use univox_core::session::{Session, SessionManager};
use univox_core::id::MemberId;
use univox_core::{ChannelOptions, ConnectOptions, Credential};
use univox_ts3::{Ts3ConnectOptions, Ts3Driver, Ts3Ext, Ts3Session};
use univox_ts3_proto::{Command, Identity, RowExt};

async fn spawn_session(nickname: &str) -> (Ts3Server, std::sync::Arc<Ts3Session>) {
    let server = Ts3Server::start().await.expect("boot");
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous)
        .bookkeeping(univox_core::BookkeepingConfig {
            enabled: true,
            member_states: true,
        });
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("session connect");
    (server, session)
}

#[tokio::test(flavor = "multi_thread")]
async fn session_book_and_events() {
    let (server, session) = spawn_session("Book Bot").await;
    let _keep = server;

    // The book mirror fills from the login dumps.
    let mut ch_count = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && ch_count == 0 {
        ch_count = session.book().channels().len();
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(ch_count >= 1, "book has no channels after login");

    // The self member is registered.
    let self_known = session
        .book()
        .with(|b| b.self_member.member_id.is_some())
        .unwrap_or(false);
    assert!(self_known, "self member unknown");

    // Send a channel message; our own message is echoed by the server.
    session
        .send_message(
            MessageTarget::Channel(1u64.into()),
            &univox_core::message::MessageContent::Plain("hello book".into()),
        )
        .await
        .expect("send_message");

    session.disconnect(Some("done".to_string())).await.expect("disconnect");
}

#[tokio::test(flavor = "multi_thread")]
async fn session_two_clients_see_each_other() {
    let server = Ts3Server::start().await.expect("boot");
    let addr = format!("127.0.0.1:{}", server.voice_port);

    let mk_opts = |name: &str| {
        ConnectOptions::new(addr.clone())
            .nickname(name)
            .credential(Credential::Anonymous)
            .bookkeeping(univox_core::BookkeepingConfig::default())
    };

    let a = Ts3Session::connect(mk_opts("Bot A"), Identity::create())
        .await
        .expect("connect A");
    let mut a_events = a.events();
    tokio::time::sleep(Duration::from_millis(300)).await;

    let b = Ts3Session::connect(mk_opts("Bot B"), Identity::create())
        .await
        .expect("connect B");
    let _ = &b;

    // A's book should learn about B via notifycliententerview.
    let mut b_member_id = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
    while tokio::time::Instant::now() < deadline {
        for member in a.book().members() {
            if member.nickname == "Bot B" {
                b_member_id = Some(member.id.clone());
            }
        }
        if b_member_id.is_some() {
            break;
        }
        if let Ok(Some(ev)) =
            tokio::time::timeout(Duration::from_millis(300), a_events.next()).await
        {
            if let Event::MemberJoined { member } = ev.as_ref() {
                if member.nickname == "Bot B" {
                    b_member_id = Some(member.id.clone());
                }
            }
        }
    }
    let b_id = b_member_id.expect("A's book never saw Bot B");

    // Send a channel message from B (both bots sit in the default channel);
    // a server-wide message would need b_client_server_textmessage_send,
    // which guests lack (permid 218).
    b.send_message(
        MessageTarget::Channel(1u64.into()),
        &univox_core::message::MessageContent::Plain("announcement".to_string()),
    )
    .await
    .expect("B sends");

    let mut got_text = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !got_text {
        match tokio::time::timeout(Duration::from_millis(400), a_events.next()).await {
            Ok(Some(ev)) => {
                if let Event::MessageCreated { message } = ev.as_ref() {
                    if message.content == "announcement" {
                        got_text = true;
                    }
                }
            }
            _ => {}
        }
    }
    assert!(got_text, "A did not receive B's announcement");

    a.disconnect(Some("bye".to_string())).await.ok();
    let _ = b_id;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_channel_management() {
    let server = Ts3Server::start().await.expect("boot");
    let addr = format!("127.0.0.1:{}", server.voice_port);
    let opts = ConnectOptions::new(addr)
        .nickname("Admin Bot")
        .credential(Credential::Anonymous);
    let session = Ts3Session::connect(opts, Identity::create()).await.expect("connect");

    // Use the ServerAdmin privilege key so we may create channels.
    session
        .use_privilege_key(&server.admin_token)
        .await
        .expect("tokenuse");

    let cid = session
        .create_channel(ChannelOptions {
            name: "Univox Managed".into(),
            parent: None,
            permanence: Permanence::SemiPermanent,
            topic: Some("made by univox".into()),
            ..Default::default()
        })
        .await
        .expect("create_channel");

    // The channel appears in the book after the server pushes the update.
    let mut found = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !found {
        found = session
            .book()
            .channel(&cid)
            .map(|c| c.name == "Univox Managed")
            .unwrap_or(false);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(found, "created channel missing from book");

    session.delete_channel(&cid, true).await.expect("delete_channel");

    session.disconnect(Some("done".to_string())).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn session_manager_routes_ts3() {
    let server = Ts3Server::start().await.expect("boot");
    let manager = SessionManager::new();
    manager.register_driver(std::sync::Arc::new(Ts3Driver));

    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("Managed Bot")
        .credential(Credential::Anonymous);
    let session = manager
        .connect(univox_core::SessionRequest::new(
            univox_core::platform::Platform::Ts3,
            opts,
        ))
        .await
        .expect("manager connect");

    assert_eq!(session.platform(), univox_core::platform::Platform::Ts3);
    // Default session request tag: None; set and find.
    session.set_tag(Some("main".into()));
    let found = manager.find_by_tag("main").expect("find by tag");
    assert_eq!(found.id(), session.id());
    assert!(manager.sessions().len() == 1);

    session.disconnect(Some("bye".to_string())).await.ok();
}

/// Regression: the server's ack packets arrive UNENCRYPTED (flag 0x80,
/// MAC = SharedMac, payload = the acked packet id) from the second ack on.
/// They were undecryptable before, so the ack of the first post-connect
/// command was dropped, its pending entry stranded through 12 resends, and
/// the resend give-up killed the connection ~2.5 minutes into every session.
#[tokio::test(flavor = "multi_thread")]
async fn session_command_acks_are_processed() {
    let server = Ts3Server::start().await.expect("boot");
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("Ack Bot")
        .credential(Credential::Anonymous);
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect");

    // The clientlist from prime_book must be acked shortly after connect.
    let mut pending = usize::MAX;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        pending = session.stats().pending_commands;
        if pending == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(pending, 0, "command acks are not being processed");

    session.disconnect(Some("done".to_string())).await.ok();
}

/// Regression: with `-away`, the clientlist response rows lead with a bare
/// `client_away_message` key whenever no listed client is away (the server
/// omits the `=` for empty values). The wire parser then mistakes the bare
/// key for a command name, the rows failed the response-name match, and the
/// dump came back as `Ok` with zero rows — the roster stayed empty.
#[tokio::test(flavor = "multi_thread")]
async fn session_clientlist_away_dump_populates_roster() {
    let server = Ts3Server::start().await.expect("boot");
    let addr = format!("127.0.0.1:{}", server.voice_port);

    // Guests lack the view permission (permid 27), so take the admin token
    // at connect — prime_book's clientlist fires inside connect().
    let opts = ConnectOptions::new(addr)
        .nickname("Roster Bot")
        .credential(Credential::Anonymous)
        .bookkeeping(univox_core::BookkeepingConfig {
            enabled: true,
            member_states: true,
        })
        .with_extension(Ts3ConnectOptions {
            privilege_key: Some(server.admin_token.clone()),
            ..Default::default()
        });
    let session = Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect");

    // The dump ran inside connect: the self row must reach the book.
    let mut self_in_roster = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !self_in_roster {
        self_in_roster = session
            .book()
            .members()
            .iter()
            .any(|m| m.nickname == "Roster Bot");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(self_in_roster, "clientlist dump left the roster empty");

    // The same command by hand must return rows, not an empty Ok.
    let rows = session
        .exec(
            Command::new("clientlist")
                .opt("uid")
                .opt("away")
                .opt("voice")
                .opt("groups"),
        )
        .await
        .expect("clientlist");
    let clid = univox_ts3::self_clid(&session).to_string();
    assert!(
        rows.iter().any(|r| r.get("clid") == Some(clid.as_str())),
        "clientlist returned no rows for self"
    );

    session.disconnect(Some("done".to_string())).await.ok();
}

/// Whisper lists are pure client-side state: CRUD, activation and the
/// guard rails (FEATURES.md §6.4). No server interaction involved.
#[tokio::test(flavor = "multi_thread")]
async fn whisper_list_crud_and_activation() {
    let (_server, session) = spawn_session("Whisper List Bot").await;

    assert!(session.whisper_lists().is_empty());
    assert_eq!(session.active_whisper_list().await, None);

    let a = session
        .add_whisper_list(
            "admins",
            vec![univox_ts3::WhisperTarget::Member(5u64.into())],
        )
        .await
        .expect("add list a");
    let b = session
        .add_whisper_list(
            "lobby",
            vec![univox_ts3::WhisperTarget::Channel(1u64.into())],
        )
        .await
        .expect("add list b");
    assert_ne!(a, b);

    let lists = session.whisper_lists();
    assert_eq!(lists.len(), 2);
    let admins = lists.iter().find(|l| l.id == a).expect("list a");
    assert_eq!(admins.name, "admins");
    assert_eq!(admins.targets.len(), 1);

    session.set_active_whisper_list(Some(b)).await.expect("activate");
    assert_eq!(session.active_whisper_list().await, Some(b));

    // Removing the active list also deactivates it.
    session.remove_whisper_list(b).await.expect("remove b");
    assert_eq!(session.active_whisper_list().await, None);

    // Unknown ids error on remove/activate; sending without an active
    // list errors too.
    assert!(session.remove_whisper_list(999).await.is_err());
    assert!(session.set_active_whisper_list(Some(999)).await.is_err());
    assert!(session.send_whisper_to_active_list(&[0u8; 4]).await.is_err());

    // Empty or oversized target sets are rejected at add time.
    assert!(session.add_whisper_list("empty", vec![]).await.is_err());
    let too_many: Vec<univox_ts3::WhisperTarget> = (0..66)
        .map(|i| univox_ts3::WhisperTarget::Member(MemberId::from_u64(i)))
        .collect();
    assert!(session.add_whisper_list("big", too_many).await.is_err());

    session.disconnect(Some("done".to_string())).await.ok();
}
