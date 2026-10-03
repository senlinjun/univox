//! Unified Ts3Session integration tests: book mirror, events, messaging.

use std::time::Duration;

use test_support::Ts3Server;
use univox_core::event::Event;
use univox_core::model::{MessageTarget, Permanence};
use univox_core::session::{Driver, Session, SessionManager};
use univox_core::{BookConfig, ChannelOptions, ConnectOptions, Credential};
use univox_ts3::{Ts3Driver, Ts3Session};
use univox_ts3_proto::Identity;

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
