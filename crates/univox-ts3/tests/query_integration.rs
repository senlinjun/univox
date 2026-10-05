//! ServerQuery integration tests against the bundled local TS3 server
//! (3.13.x). Empirical server behaviors verified against 3.13.8:
//!
//! - lines are `\r`-prefixed and `\n`-terminated
//! - multi-row responses arrive as ONE line with `|` row separators
//! - per-command flood limits apply even to allowlisted loopback IPs
//!   (10 cmds / 3s) — tests raise the budget via `instanceedit`
//! - `channelmove` within the same parent errors 770; a parent change works
//! - query accounts cannot join server groups (512) nor receive offline
//!   messages (512); both need a real client connection (M4)
//! - `servernotifyregister event=channel` requires an `id`
//! - `queryloginadd` requires `use 0` (global logins) unless cldbid is given
//! - `serversnapshotcreate` returns `version=3 data=...` (no `snapshot` key)

use std::time::Duration;

use test_support::Ts3Server;
use univox_ts3_proto::{Command, Error, RowExt};
use univox_ts3::{QueryOptions, QuerySession};

async fn spawn_server() -> Ts3Server {
    Ts3Server::start()
        .await
        .expect("failed to boot local ts3server")
}

async fn admin_on(port: u16, password: &str) -> QuerySession {
    let q = QuerySession::connect(QueryOptions {
        port,
        username: Some("serveradmin".into()),
        password: Some(password.to_string()),
        server: Some(1),
        ..Default::default()
    })
    .await
    .expect("admin query session");
    // Raise the per-command flood budget (allowlist does not exempt it).
    q.exec(
        Command::new("instanceedit")
            .param("serverinstance_serverquery_flood_commands", 9999)
            .param("serverinstance_serverquery_flood_time", 1),
    )
    .await
    .expect("instanceedit flood budget");
    q
}

async fn default_channel(q: &QuerySession) -> u64 {
    let channels = q.exec(Command::new("channellist")).await.expect("channellist");
    channels[0]
        .get("cid")
        .unwrap_or_default()
        .parse()
        .expect("cid")
}

fn server_err(e: &Error) -> i32 {
    e.server_id().unwrap_or(-1)
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_login_use_and_basics() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let me = q.whoami().await.expect("whoami");
    assert!(me.has("client_login_name", "serveradmin"));
    assert!(me.has("client_unique_identifier", "serveradmin"));

    let servers = q.exec(Command::new("serverlist")).await.expect("serverlist");
    assert_eq!(servers[0].get("virtualserver_id"), Some("1"));
    let info = q
        .exec_one(Command::new("serverinfo"))
        .await
        .expect("serverinfo");
    assert!(info.contains_key("virtualserver_name"));

    let channels = q.exec(Command::new("channellist")).await.expect("channellist");
    assert!(channels.len() >= 1);
    assert!(channels[0].contains_key("channel_name"));
    let clients = q.exec(Command::new("clientlist")).await.expect("clientlist");
    assert!(!clients.is_empty());
    let found = q
        .exec_one(Command::new("channelfind").param("pattern", "Default"))
        .await
        .expect("channelfind");
    assert!(found.contains_key("cid"));

    let v = q.version().await.expect("version");
    assert!(v.contains_key("version"));
    let host = q.exec_one(Command::new("hostinfo")).await.expect("hostinfo");
    assert!(host.contains_key("instance_uptime"));
    let inst = q
        .exec_one(Command::new("instanceinfo"))
        .await
        .expect("instanceinfo");
    assert!(inst.contains_key("serverinstance_database_version"));
    let bindings = q.exec(Command::new("bindinglist")).await.expect("bindinglist");
    assert!(!bindings.is_empty());

    let sid = q
        .exec_one(Command::new("serveridgetbyport").param("virtualserver_port", server.voice_port))
        .await
        .expect("serveridgetbyport");
    assert_eq!(sid.get("server_id"), Some("1"));

    // Bad login is rejected with 520.
    let bad = QuerySession::connect(QueryOptions {
        port: server.query_port,
        username: Some("serveradmin".into()),
        password: Some("wrong".into()),
        ..Default::default()
    })
    .await;
    match bad {
        Err(Error::Server { id: 520, .. }) => {}
        Err(e) => panic!("expected 520 login failed, got {e}"),
        Ok(_) => panic!("bad login accepted"),
    }

    q.quit().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn channel_lifecycle() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let created = q
        .exec_one(
            Command::new("channelcreate")
                .param("channel_name", "Test Channel|Alpha")
                .param("cpid", 0)
                .param("channel_flag_permanent", 1)
                .param("channel_maxclients", 10),
        )
        .await
        .expect("channelcreate");
    let cid: u64 = created.get("cid").unwrap_or_default().parse().expect("cid");

    let info = q
        .exec_one(Command::new("channelinfo").param("cid", cid))
        .await
        .expect("channelinfo");
    assert_eq!(
        info.get("channel_name"),
        Some("Test Channel|Alpha"),
        "channel name round-trip"
    );
    q.exec(
        Command::new("channeledit")
            .param("cid", cid)
            .param("channel_description", "Created by univox test"),
    )
    .await
    .expect("channeledit");

    // Parent-change move works; same-parent reorder errors 770.
    let sub = q
        .exec_one(
            Command::new("channelcreate")
                .param("channel_name", "Sub")
                .param("cpid", 0)
                .param("channel_flag_semi_permanent", 1),
        )
        .await
        .expect("sub channel");
    let sub_cid: u64 = sub.get("cid").unwrap_or_default().parse().unwrap();
    q.exec(
        Command::new("channelmove")
            .param("cid", sub_cid)
            .param("cpid", cid)
            .param("order", 0),
    )
    .await
    .expect("channelmove to parent");
    let channels = q.exec(Command::new("channellist")).await.unwrap();
    let moved = channels
        .iter()
        .find(|row| row.has("cid", &sub_cid.to_string()))
        .expect("moved channel");
    assert_eq!(moved.get("pid"), Some(&cid.to_string()).map(String::as_str));
    let _ = q
        .exec(Command::new("channelmove").param("cid", cid).param("cpid", 0).param("order", 0))
        .await
        .map(|_| assert!(false, "same-parent move should fail"))
        .map_err(|e: Error| assert_eq!(server_err(&e), 770));

    let groups = q.exec(Command::new("channelgrouplist")).await.expect("channelgrouplist");
    assert!(groups.len() >= 2);
    q.exec(Command::new("channelpermlist").param("cid", cid))
        .await
        .expect("channelpermlist");

    q.exec(Command::new("channeldelete").param("cid", sub_cid).param("force", 1))
        .await
        .expect("delete sub");
    q.exec(Command::new("channeldelete").param("cid", cid))
        .await
        .expect("delete parent");

    let err = q
        .exec_one(Command::new("channelinfo").param("cid", 99999))
        .await
        .unwrap_err();
    assert_eq!(server_err(&err), 768);
}

#[tokio::test(flavor = "multi_thread")]
async fn groups_and_permissions() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let groups = q.exec(Command::new("servergrouplist")).await.expect("servergrouplist");
    assert!(groups.iter().any(|row| row.has("name", "Guest")));
    let _admin_group: u64 = groups
        .iter()
        .find(|row| row.has("name", "Server Admin"))
        .and_then(|row| row.get("sgid").and_then(|v| v.parse().ok()))
        .expect("Server Admin group");

    let perms = q.exec(Command::new("permissionlist")).await.expect("permissionlist");
    assert!(perms.len() > 300, "expected ~400 permissions, got {}", perms.len());
    let pid = q
        .exec_one(Command::new("permidgetbyname").param("permsid", "b_client_ignore_bans"))
        .await
        .expect("permidgetbyname");
    assert!(pid.contains_key("permid"), "row: {pid:?}");

    let g = q
        .exec_one(
            Command::new("servergroupadd")
                .param("name", "UnivoxTest")
                .param("type", 1),
        )
        .await
        .expect("servergroupadd");
    let sgid: u64 = g.get("sgid").unwrap_or_default().parse().unwrap();

    q.exec(Command::new("servergrouprename").param("sgid", sgid).param("name", "UnivoxRenamed"))
        .await
        .expect("servergrouprename");

    q.exec(
        Command::new("servergroupaddperm")
            .param("sgid", sgid)
            .param("permsid", "b_client_ignore_bans")
            .param("permvalue", 1)
            .param("permnegated", 0)
            .param("permskip", 0),
    )
    .await
    .expect("servergroupaddperm");
    let perm_id = q
        .exec_one(Command::new("permidgetbyname").param("permsid", "b_client_ignore_bans"))
        .await
        .expect("permidgetbyname");
    let perm_id = perm_id.get("permid").unwrap_or_default().to_string();
    let gperms = q
        .exec(Command::new("servergrouppermlist").param("sgid", sgid))
        .await
        .expect("servergrouppermlist");
    assert!(
        gperms.iter().any(|row| row.has("permid", &perm_id)),
        "added permission missing from group permlist"
    );
    q.exec(
        Command::new("servergroupdelperm")
            .param("sgid", sgid)
            .param("permsid", "b_client_ignore_bans"),
    )
    .await
    .expect("servergroupdelperm");

    // Query accounts cannot be added to server groups (needs a real client).
    let me = q.whoami().await.expect("whoami");
    let cldbid: u64 = me.get("client_database_id").unwrap_or_default().parse().unwrap();
    let err = q
        .exec(
            Command::new("servergroupaddclient")
                .param("sgid", sgid)
                .param("cldbid", cldbid),
        )
        .await
        .unwrap_err();
    assert_eq!(server_err(&err), 512, "query cldbid cannot join groups");

    let _ = q
        .exec(Command::new("servergroupsbyclientid").param("cldbid", cldbid))
        .await; // empty result set errors — acceptable for query accounts

    q.exec(Command::new("servergroupdel").param("sgid", sgid))
        .await
        .expect("servergroupdel");

    let found = q
        .exec(Command::new("permfind").param("permsid", "b_virtualserver_start"))
        .await
        .expect("permfind");
    assert!(!found.is_empty());

    let cid = default_channel(&q).await;
    let overview = q
        .exec(
            Command::new("permoverview")
                .param("cid", cid)
                .param("cldbid", cldbid)
                .param("permid", 0),
        )
        .await
        .expect("permoverview");
    assert!(!overview.is_empty());

    let got = q
        .exec_one(Command::new("permget").param("permsid", "b_virtualserver_start"))
        .await
        .expect("permget");
    assert!(got.contains_key("permvalue"));
}

#[tokio::test(flavor = "multi_thread")]
async fn moderation_privilegekeys_and_custom() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let ban = q
        .exec_one(Command::new("banadd").param("name", "bad guy").param("time", 60))
        .await
        .expect("banadd");
    let banid: u64 = ban.get("banid").unwrap_or_default().parse().unwrap();
    let bans = q.exec(Command::new("banlist")).await.expect("banlist");
    assert!(bans.iter().any(|row| row.has("name", "bad guy")));
    q.exec(Command::new("bandel").param("banid", banid)).await.expect("bandel");
    q.exec(Command::new("bandelall")).await.expect("bandelall");

    let me = q.whoami().await.unwrap();
    let cldbid: u64 = me.get("client_database_id").unwrap_or_default().parse().unwrap();
    q.exec(
        Command::new("complainadd")
            .param("tcldbid", cldbid)
            .param("fcldbid", cldbid)
            .param("message", "test complaint"),
    )
    .await
    .expect("complainadd");
    let complaints = q
        .exec(Command::new("complainlist").param("tcldbid", cldbid))
        .await
        .expect("complainlist");
    assert!(!complaints.is_empty());
    q.exec(
        Command::new("complaindel")
            .param("tcldbid", cldbid)
            .param("fcldbid", cldbid),
    )
    .await
    .expect("complaindel");

    // privilege keys
    let key = q
        .exec_one(
            Command::new("privilegekeyadd")
                .param("tokentype", 0)
                .param("tokenid1", 6)
                .param("tokenid2", 0)
                .param("description", "univox test key"),
        )
        .await
        .expect("privilegekeyadd");
    let token = key.get("token").unwrap_or_default().to_string();
    assert!(!token.is_empty());
    let keys = q.exec(Command::new("privilegekeylist")).await.expect("privilegekeylist");
    assert!(keys.iter().any(|row| row.has("token", &token)));
    q.exec(Command::new("privilegekeydelete").param("token", token.clone()))
        .await
        .expect("privilegekeydelete");

    // Offline messages need a real client (query accounts → 512).
    let res = q
        .exec(
            Command::new("messageadd")
                .param("cluid", "serveradmin")
                .param("subject", "univox subject")
                .param("message", "offline hello"),
        )
        .await;
    match res {
        Err(Error::Server { id: 512, .. }) => {}
        Err(e) => panic!("messageadd unexpected error: {e}"),
        Ok(_) => unreachable!(),
    }
    // Empty inbox → 1281 "database empty result set"; both are acceptable.
    match q.exec(Command::new("messagelist")).await {
        Err(Error::Server { id: 1281, .. }) | Ok(_) => {}
        Err(e) => panic!("messagelist unexpected error: {e}"),
    }

    // custom fields
    q.exec(
        Command::new("customset")
            .param("cldbid", cldbid)
            .param("ident", "univox_test")
            .param("value", "42"),
    )
    .await
    .expect("customset");
    let info = q
        .exec(Command::new("custominfo").param("cldbid", cldbid))
        .await
        .expect("custominfo");
    assert!(info.iter().any(|row| row.has("ident", "univox_test")));
    let search = q
        .exec(
            Command::new("customsearch")
                .param("ident", "univox_test")
                .param("pattern", "4%"),
        )
        .await
        .expect("customsearch");
    assert!(!search.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn clientdb_and_id_mapping() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let list = q
        .exec(Command::new("clientdblist").opt("count"))
        .await
        .expect("clientdblist");
    assert!(!list.is_empty());
    let cldbid: u64 = list[0].get("cldbid").unwrap_or_default().parse().unwrap();

    let info = q
        .exec_one(Command::new("clientdbinfo").param("cldbid", cldbid))
        .await
        .expect("clientdbinfo");
    assert!(info.contains_key("client_unique_identifier"));
    let uid = info.get("client_unique_identifier").unwrap_or_default().to_string();

    let found = q
        .exec(Command::new("clientdbfind").param("pattern", &uid).opt("uid"))
        .await
        .expect("clientdbfind");
    assert!(found.iter().any(|row| row.has("cldbid", &cldbid.to_string())));

    let ids = q
        .exec(Command::new("clientgetids").param("cluid", "serveradmin"))
        .await
        .expect("clientgetids");
    assert!(ids.iter().any(|row| row.has("cluid", "serveradmin")));

    if let Ok(name) = q
        .exec_one(Command::new("clientgetnamefromdbid").param("cldbid", cldbid))
        .await
    {
        assert!(name.contains_key("name"));
    }
    let _ = q
        .exec_one(Command::new("clientgetuidfromclid").param("clid", 1))
        .await;
    let _ = q
        .exec_one(Command::new("clientgetnamefromuid").param("cluid", &uid))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn notifications_between_two_query_sessions() {
    let server = spawn_server().await;
    let a = admin_on(server.query_port, &server.serveradmin_password).await;
    let b = admin_on(server.query_port, &server.serveradmin_password).await;

    let mut rx = a.notifications();
    a.notify_register("server", None).await.expect("register server");
    a.notify_register("textserver", None).await.expect("register textserver");
    // `event=channel id=0` covers ALL channels (id omitted errors 1539 on
    // 3.13.x; id=0 delivers notifychannelcreated/deleted/moved).
    let _ = default_channel(&a).await;
    a.notify_register("channel", Some(0)).await.expect("register channel");
    tokio::time::sleep(Duration::from_millis(300)).await;

    b.send_text_message(3, 0, "hello from b").await.expect("sendtextmessage");
    b.exec(Command::new("channelcreate").param("channel_name", "NotifTest"))
        .await
        .expect("channelcreate");

    let mut got_text = false;
    let mut got_channel = false;
    let mut seen: Vec<String> = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::time::Instant::now() < deadline && !(got_text && got_channel) {
        match tokio::time::timeout(Duration::from_millis(1500), rx.recv()).await {
            Ok(Ok(cmd)) => {
                seen.push(cmd.name.clone());
                if cmd.name == "notifytextmessage" {
                    assert!(cmd.get("msg").unwrap_or("").contains("hello from b"));
                    got_text = true;
                } else if cmd.name.starts_with("notifychannel") {
                    got_channel = true;
                }
            }
            Ok(Err(_)) | Err(_) => break,
        }
    }
    assert!(got_text, "no text notification received; seen: {seen:?}");
    assert!(got_channel, "no channel notification received; seen: {seen:?}");

    a.notify_unregister().await.expect("unregister");
}

#[tokio::test(flavor = "multi_thread")]
async fn snapshots_logs_querylogins_apikeys_and_temp_passwords() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;

    let snap = q
        .exec_one(Command::new("serversnapshotcreate"))
        .await
        .expect("serversnapshotcreate");
    assert!(
        snap.contains_key("data") || snap.contains_key("snapshot"),
        "snapshot row: {snap:?}"
    );

    q.exec(
        Command::new("logadd")
            .param("loglevel", 4)
            .param("logmsg", "univox was here"),
    )
    .await
    .expect("logadd");
    let logs = q.exec(Command::new("logview").param("lines", 10)).await.expect("logview");
    assert!(!logs.is_empty());

    // Query logins are global: deselect the server first (`use 0`).
    q.use_server(0, false).await.expect("use 0");
    let ql = q
        .exec_one(Command::new("queryloginadd").param("client_login_name", "univox_query"))
        .await
        .expect("queryloginadd");
    assert!(ql.contains_key("client_login_password"), "row: {ql:?}");
    let list = q
        .exec(Command::new("queryloginlist").opt("count"))
        .await
        .expect("queryloginlist");
    assert!(list.iter().any(|row| row.has("client_login_name", "univox_query")));
    q.exec(Command::new("querylogindel").param("cldbid", ql.get("cldbid").unwrap_or_default()))
        .await
        .expect("querylogindel");
    q.use_server(1, false).await.expect("use 1");

    let key = q
        .exec_one(Command::new("apikeyadd").param("scope", "read").param("lifetime", 3600))
        .await
        .expect("apikeyadd");
    assert!(key.contains_key("apikey"));
    let keys = q.exec(Command::new("apikeylist")).await.expect("apikeylist");
    assert!(!keys.is_empty());
    q.exec(Command::new("apikeydel").param("id", key.get("id").unwrap_or_default()))
        .await
        .expect("apikeydel");

    q.exec(
        Command::new("servertemppasswordadd")
            .param("pw", "temppw123")
            .param("duration", 300)
            .param("desc", "univox"),
    )
    .await
    .expect("servertemppasswordadd");
    let tps = q
        .exec(Command::new("servertemppasswordlist"))
        .await
        .expect("servertemppasswordlist");
    assert!(!tps.is_empty());
    q.exec(Command::new("servertemppassworddel").param("pw", "temppw123"))
        .await
        .expect("servertemppassworddel");

    // serveredit round-trip
    let orig = q.exec_one(Command::new("serverinfo")).await.unwrap();
    let orig_name = orig.get("virtualserver_name").unwrap_or_default().to_string();
    q.exec(Command::new("serveredit").param("virtualserver_name", "Univox Renamed"))
        .await
        .expect("serveredit");
    q.exec(Command::new("serveredit").param("virtualserver_name", orig_name))
        .await
        .expect("restore name");

    // server group copy
    let groups = q.exec(Command::new("servergrouplist")).await.unwrap();
    let guest: u64 = groups
        .iter()
        .find(|row| row.has("name", "Guest"))
        .and_then(|row| row.get("sgid").and_then(|v| v.parse().ok()))
        .unwrap();
    let copied = q
        .exec_one(
            Command::new("servergroupcopy")
                .param("ssgid", guest)
                .param("tsgid", 0)
                .param("name", "Copied Group")
                .param("type", 1),
        )
        .await
        .expect("servergroupcopy");
    let copied_sgid: u64 = copied.get("sgid").unwrap_or_default().parse().unwrap();
    q.exec(Command::new("servergroupdel").param("sgid", copied_sgid))
        .await
        .ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn keepalive_and_graceful_quit() {
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;
    q.quit().await.ok();
    assert!(q.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn serveredit_password_sets_flag() {
    // Empirical: `serveredit virtualserver_password=...` alone is accepted
    // and implies `virtualserver_flag_password=1`; sending the flag
    // explicitly errors 1538 (invalid parameter) on 3.13.
    let server = spawn_server().await;
    let q = admin_on(server.query_port, &server.serveradmin_password).await;
    q.exec(Command::new("serveredit").param("virtualserver_password", "s3cret"))
        .await
        .expect("serveredit password");
    let info = q.exec_one(Command::new("serverinfo")).await.unwrap();
    assert_eq!(info.get("virtualserver_flag_password"), Some("1"));
}
