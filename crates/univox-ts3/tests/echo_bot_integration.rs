// TEMP acceptance: boot server, run echo bot logic in-process, verify echo.
use std::sync::Arc;
use std::time::Duration;
use test_support::Ts3Server;
use univox_core::event::Event;
use univox_core::message::MessageContent;
use univox_core::model::MessageTarget;
use univox_core::session::{Driver, SessionManager};
use univox_core::{ConnectOptions, Credential, SessionRequest};
use univox_ts3::Ts3Driver;

#[tokio::test(flavor = "multi_thread")]
async fn echo_bot_acceptance() {
    let server = Ts3Server::start().await.expect("boot");
    let manager = SessionManager::new();
    manager.register_driver(Arc::new(Ts3Driver));

    // The bot, via the manager + invite-style address (exercises parsing).
    let bot_opts = ConnectOptions::new(format!("ts3server://127.0.0.1?port={}&nickname=EchoBot", server.voice_port))
        .credential(Credential::Anonymous);
    let bot = manager
        .connect(SessionRequest::new(univox_core::platform::Platform::Ts3, bot_opts))
        .await
        .expect("bot connect");
    println!("BOT connected: {:?}", bot.state());
    assert_eq!(bot.state(), univox_core::session::SessionState::Connected);
    let mut bot_events = bot.events();

    // A peer that sends and listens.
    let peer_opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname("Peer")
        .credential(Credential::Anonymous);
    let peer = Ts3Driver.connect(peer_opts).await.expect("peer connect");
    let mut peer_events = peer.events();
    tokio::time::sleep(Duration::from_millis(300)).await;

    peer.send_message(
        MessageTarget::Channel(1u64.into()),
        &MessageContent::Plain("hello bot".to_string()),
    )
    .await
    .expect("peer send");

    let mut got = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !got {
        if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), bot_events.next()).await {
            if let Event::MessageCreated { message } = ev.as_ref() {
                if message.content == "hello bot" {
                    got = true;
                }
            }
        }
    }
    assert!(got, "bot never saw the peer message");

    // The bot echoes; the peer sees it.
    bot.send_message(
        MessageTarget::Channel(1u64.into()),
        &MessageContent::Plain("echo: hello bot".to_string()),
    )
    .await
    .expect("bot echo");
    let mut echoed = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !echoed {
        if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_millis(300), peer_events.next()).await {
            if let Event::MessageCreated { message } = ev.as_ref() {
                if message.content == "echo: hello bot" {
                    echoed = true;
                }
            }
        }
    }
    assert!(echoed, "peer never saw the echo");
    println!("ECHO ACCEPTANCE PASSED");
    peer.disconnect(None).await.ok();
    bot.disconnect(None).await.ok();
}
