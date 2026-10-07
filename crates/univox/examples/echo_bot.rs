//! Univox echo bot (TeamSpeak 3): connects to a server, echoes channel
//! messages back, pokes new members and logs events.
//!
//! Usage:
//!   cargo run -p univox --example echo_bot -- <address> [nickname]
//!
//! `<address>` can be `host:port`, a bare host (port resolved via
//! TSDNS), or an invite link (`ts3server://…`).

use std::sync::Arc;
use std::time::Duration;

use univox::{
    ChannelId, ConnectOptions, Credential, Event, MessageContent, MessageTarget, Platform,
    SessionManager, SessionRequest, Ts3Driver,
};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let mut args = std::env::args().skip(1);
    let address = args.next().unwrap_or_else(|| "127.0.0.1:9987".into());
    let nickname = args.next().unwrap_or_else(|| "UnivoxEchoBot".into());

    let manager = SessionManager::new();
    manager.register_driver(Arc::new(Ts3Driver));

    let opts = ConnectOptions::new(&address)
        .nickname(&nickname)
        .credential(Credential::Anonymous);
    let session = manager
        .connect(SessionRequest::new(Platform::Ts3, opts))
        .await?;
    println!("connected: {} (state: {:?})", session.id(), session.state());

    let mut events = session.events();
    loop {
        let Some(ev) = events.next().await else { break };
        match ev.as_ref() {
            Event::Connected => println!("ready"),
            Event::MemberJoined { member } => {
                println!("+ {}", member.nickname);
                let _ = session.poke(&member.id, "welcome! (univox echo bot)").await;
            }
            Event::MemberLeft { id, .. } => println!("- {id}"),
            Event::MessageCreated { message } => {
                let author = message
                    .author
                    .as_ref()
                    .map(|a| a.as_u64().unwrap_or(0))
                    .unwrap_or(0);
                println!("<{}> {}", message.author_name, message.content);
                // Echo back into the same channel (skip our own messages).
                if author != 0 && message.content != ".quit" {
                    let _ = session
                        .send_message(
                            MessageTarget::Channel(ChannelId::from_u64(1)),
                            &MessageContent::Plain(format!("echo: {}", message.content)),
                        )
                        .await;
                }
                if message.content == ".quit" {
                    break;
                }
            }
            Event::Closed { reason } => {
                println!("closed: {reason:?}");
                break;
            }
            _ => {}
        }
    }

    session.disconnect(Some("echo bot leaving".into())).await?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}
