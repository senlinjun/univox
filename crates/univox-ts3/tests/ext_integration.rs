//! Ts3Ext integration tests: ID mapping, client database, offline
//! messages, bans, complaints, self update, plugin commands
//! (FEATURES.md §7.6/§9/§10/§11).

use std::sync::Arc;
use std::time::Duration;

use test_support::Ts3Server;
use univox_core::event::Event;
use univox_core::id::MemberId;
use univox_core::session::Session;
use univox_core::{ConnectOptions, Credential};
use univox_ts3::ext::{SelfUpdate, Ts3Ext};
use univox_ts3::{self_clid, Ts3Session};
use univox_ts3_proto::{Identity, RowExt};

async fn spawn_session(server: &Ts3Server, nickname: &str) -> Arc<Ts3Session> {
    let opts = ConnectOptions::new(format!("127.0.0.1:{}", server.voice_port))
        .nickname(nickname)
        .credential(Credential::Anonymous);
    Ts3Session::connect(opts, Identity::create())
        .await
        .expect("connect")
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_id_mapping_and_clientdb() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_session(&server, "Mapper").await;
    session.use_privilege_key(&server.admin_token).await.expect("token");

    let me = MemberId::from_u64(self_clid(&session));
    let uid = session.uid_from_clid(&me).await.expect("uid_from_clid");
    assert!(!uid.is_empty(), "empty uid");

    let dbid = session.dbid_from_uid(&uid).await.expect("dbid_from_uid").expect("no dbid");
    assert!(dbid.as_u64().unwrap_or(0) > 0);

    let name = session.name_from_uid(&uid).await.expect("name_from_uid");
    assert_eq!(name.as_deref(), Some("Mapper"));

    let name = session.name_from_dbid(&dbid).await.expect("name_from_dbid");
    assert_eq!(name.as_deref(), Some("Mapper"));

    let found = session.find_clients("Mapper").await.expect("clientfind");
    // clientfind rows carry clid + nickname only (no uid).
    assert!(
        found.iter().any(|m| m.member == me && m.nickname == "Mapper"),
        "self not in clientfind: {found:?}"
    );

    let entries = session.client_db_list(0, 10).await.expect("clientdblist");
    assert!(
        entries.iter().any(|e| e.db_id == dbid && e.uid == uid),
        "self missing from clientdblist: {entries:?}"
    );

    let info = session.client_db_info(&dbid).await.expect("clientdbinfo");
    assert_eq!(info.get("client_unique_identifier"), Some(uid.as_str()));

    session.client_db_edit(&dbid, "univox test bot").await.expect("clientdbedit");
    let info = session.client_db_info(&dbid).await.expect("clientdbinfo");
    assert_eq!(info.get("client_description"), Some("univox test bot"));

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_offline_messages() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_session(&server, "Mailer").await;
    session.use_privilege_key(&server.admin_token).await.expect("token");

    let me = MemberId::from_u64(self_clid(&session));
    let uid = session.uid_from_clid(&me).await.expect("uid");

    session
        .send_offline_message(&uid, "univox subject", "offline hello")
        .await
        .expect("messageadd");

    let list = session.offline_messages().await.expect("messagelist");
    assert!(!list.is_empty(), "offline message not listed");
    let msg = list
        .iter()
        .find(|m| m.extra.get("subject").map(String::as_str) == Some("univox subject"))
        .expect("sent message missing from list")
        .clone();

    // Only messageget returns the body.
    let fetched = session.offline_message(&msg.id).await.expect("messageget");
    assert_eq!(fetched.content, "offline hello");

    session.delete_offline_message(&msg.id).await.expect("messagedel");
    let list = session.offline_messages().await.expect("messagelist after delete");
    assert!(list.is_empty(), "message not deleted: {list:?}");

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_bans() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_session(&server, "Warden").await;
    session.use_privilege_key(&server.admin_token).await.expect("token");

    session
        .add_ban(
            None,
            Some("badactor"),
            None,
            Some(Duration::from_secs(60)),
            Some("univox test ban"),
        )
        .await
        .expect("banadd");

    let bans = session.bans().await.expect("banlist");
    let row = bans
        .iter()
        .find(|r| r.get("name") == Some("badactor"))
        .expect("ban not listed");
    let ban_id: u64 = row.get("banid").and_then(|v| v.parse().ok()).expect("no banid");

    session.remove_ban(ban_id).await.expect("bandel");
    let bans = session.bans().await.expect("banlist after del");
    assert!(bans.iter().all(|r| r.get("name") != Some("badactor")), "ban not removed");

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_complaints() {
    let server = Ts3Server::start().await.expect("boot");
    let admin = spawn_session(&server, "Moderator").await;
    admin.use_privilege_key(&server.admin_token).await.expect("token");
    let bot = spawn_session(&server, "Spammer").await;

    // The complaint target is identified by database id (tcldbid).
    let bot_me = MemberId::from_u64(self_clid(&bot));
    let bot_uid = admin.uid_from_clid(&bot_me).await.expect("bot uid");
    let bot_dbid = admin
        .dbid_from_uid(&bot_uid)
        .await
        .expect("dbid")
        .expect("bot dbid");

    admin
        .add_complaint(&bot_dbid, "univox complaint test")
        .await
        .expect("complainadd");

    let list = admin.complaints().await.expect("complainlist");
    let row = list
        .iter()
        .find(|r| r.get("message") == Some("univox complaint test"))
        .expect("complaint not listed");
    let target_db: u64 = row.get("tcldbid").and_then(|v| v.parse().ok()).expect("no tcldbid");
    let filed_by: u64 = row.get("fcldbid").and_then(|v| v.parse().ok()).expect("no fcldbid");

    admin.delete_complaint(target_db, filed_by).await.expect("complaindel");
    let list = admin.complaints().await.expect("complainlist after del");
    assert!(
        list.iter().all(|r| r.get("message") != Some("univox complaint test")),
        "complaint not removed"
    );

    admin.disconnect(None).await.ok();
    bot.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_self_update() {
    let server = Ts3Server::start().await.expect("boot");
    let session = spawn_session(&server, "Updater").await;

    session.use_privilege_key(&server.admin_token).await.expect("token");
    session
        .update_self(SelfUpdate {
            away: Some(true),
            away_message: Some("brb".into()),
            is_channel_commander: Some(true),
            ..Default::default()
        })
        .await
        .expect("clientupdate");

    // The server echoes the change as notifyclientupdated; the book mirror
    // should carry the away message in the member row/state.
    let mut seen = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline && !seen {
        seen = session.book().with(|b| {
            b.members.values().any(|m| {
                m.nickname == "Updater"
                    && m.extra.get("client_away_message").map(String::as_str) == Some("brb")
            })
        }) == Some(true);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(seen, "clientupdate not mirrored into the book");

    session.disconnect(None).await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn ext_plugin_command() {
    let server = Ts3Server::start().await.expect("boot");
    let sender = spawn_session(&server, "Plugin Sender").await;
    let receiver = spawn_session(&server, "Plugin Receiver").await;
    let mut events = receiver.events();
    tokio::time::sleep(Duration::from_millis(300)).await;

    sender
        .send_plugin_command(
            univox_core::event::PluginCommandTarget::All,
            "univox-test",
            "plugin-payload",
            None,
        )
        .await
        .expect("plugincmd");

    let mut got = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(4);
    while tokio::time::Instant::now() < deadline && !got {
        match tokio::time::timeout(Duration::from_millis(400), events.next()).await {
            Ok(Some(ev)) => match ev.as_ref() {
                Event::PluginCommandReceived { payload, .. } => {
                    assert_eq!(std::str::from_utf8(payload), Ok("plugin-payload"));
                    got = true;
                }
                Event::Raw(raw) if raw.name == "notifyplugincmd" => {
                    got = raw
                        .payload
                        .iter()
                        .any(|(k, v)| k == "data" && v == "plugin-payload");
                }
                _ => {}
            },
            _ => {}
        }
    }
    assert!(got, "receiver never got the plugin command");

    sender.disconnect(None).await.ok();
    receiver.disconnect(None).await.ok();
}
