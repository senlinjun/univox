//! TS3 platform extension layer (FEATURES.md §9/§10/§11): the client-side
//! capabilities that have no cross-platform equivalent — ID mapping,
//! client database, offline messages, bans/complaints, runtime self state,
//! plugin commands and local muting.

use std::time::Duration;

use async_trait::async_trait;

use univox_core::error::{Error, Result};
use univox_core::event::PluginCommandTarget;
use univox_core::id::{DbId, MemberId, MessageId};
use univox_core::model::Message;
use univox_core::session::Session;
use univox_ts3_proto::{Command, Row, RowExt};

use crate::session::Ts3Session;

/// A client matched by name/pattern (`clientfind`, `customsearch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMatch {
    pub member: MemberId,
    pub uid: String,
    pub nickname: String,
}

/// An entry of the server-side client database (`clientdblist`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClientDbEntry {
    pub db_id: DbId,
    pub uid: String,
    pub nickname: String,
    pub description: String,
    pub last_connected: Option<u64>,
    pub total_connections: u64,
}

/// Runtime self-state fields for `clientupdate` (FEATURES.md §11).
#[derive(Debug, Clone, Default)]
pub struct SelfUpdate {
    pub input_muted: Option<bool>,
    pub output_muted: Option<bool>,
    pub away: Option<bool>,
    pub away_message: Option<String>,
    pub is_channel_commander: Option<bool>,
    pub is_priority_speaker: Option<bool>,
    pub is_recording: Option<bool>,
    pub badges: Option<String>,
    pub meta_data: Option<String>,
}

impl SelfUpdate {
    fn into_command(self) -> Command {
        let mut cmd = Command::new("clientupdate");
        let flag = |cmd: Command, key: &str, v: Option<bool>| {
            if let Some(v) = v {
                cmd.param(key, u8::from(v))
            } else {
                cmd
            }
        };
        cmd = flag(cmd, "client_input_muted", self.input_muted);
        cmd = flag(cmd, "client_output_muted", self.output_muted);
        cmd = flag(cmd, "client_away", self.away);
        if let Some(v) = self.away_message {
            cmd = cmd.param("client_away_message", v);
        }
        cmd = flag(cmd, "client_is_channel_commander", self.is_channel_commander);
        cmd = flag(cmd, "client_is_priority_speaker", self.is_priority_speaker);
        cmd = flag(cmd, "client_is_recording", self.is_recording);
        if let Some(v) = self.badges {
            cmd = cmd.param("client_badges", v);
        }
        if let Some(v) = self.meta_data {
            cmd = cmd.param("client_meta_data", v);
        }
        cmd
    }
}

/// TS3-specific session extensions, implemented by [`Ts3Session`].
///
/// Every method maps 1:1 to a TS3 client-protocol command; most need
/// elevated permissions (use a privilege key first).
#[async_trait]
pub trait Ts3Ext: Session {
    // ---- ID mapping (§9.2) ----

    /// `clientgetuidfromclid`.
    async fn uid_from_clid(&self, member: &MemberId) -> Result<String>;
    /// `clientgetdbidfromuid`.
    async fn dbid_from_uid(&self, uid: &str) -> Result<Option<DbId>>;
    /// `clientgetnamefromuid`.
    async fn name_from_uid(&self, uid: &str) -> Result<Option<String>>;
    /// `clientgetnamefromdbid`.
    async fn name_from_dbid(&self, db_id: &DbId) -> Result<Option<String>>;
    /// `clientfind` — clients whose nickname contains `pattern`.
    async fn find_clients(&self, pattern: &str) -> Result<Vec<ClientMatch>>;

    // ---- client database (§9.3) ----

    async fn client_db_list(&self, start: u64, limit: u64) -> Result<Vec<ClientDbEntry>>;
    async fn client_db_info(&self, db_id: &DbId) -> Result<Row>;
    async fn client_db_edit(&self, db_id: &DbId, description: &str) -> Result<()>;
    async fn client_db_delete(&self, db_id: &DbId) -> Result<()>;

    // ---- custom fields (§9.4) ----

    async fn custom_info(&self, db_id: &DbId) -> Result<Vec<(String, String)>>;
    async fn custom_search(&self, key: &str, value: &str) -> Result<Vec<Row>>;

    // ---- offline messages (§7.6) ----

    async fn offline_messages(&self) -> Result<Vec<Message>>;
    async fn send_offline_message(&self, to_uid: &str, subject: &str, text: &str) -> Result<()>;
    async fn offline_message(&self, id: &MessageId) -> Result<Message>;
    async fn delete_offline_message(&self, id: &MessageId) -> Result<()>;

    // ---- bans (§10.2) ----

    async fn bans(&self) -> Result<Vec<Row>>;
    /// Add a ban; only one of ip/name/uid needs to be set. `duration=None`
    /// means permanent.
    async fn add_ban(
        &self,
        ip: Option<&str>,
        name: Option<&str>,
        uid: Option<&str>,
        duration: Option<Duration>,
        reason: Option<&str>,
    ) -> Result<()>;
    async fn remove_ban(&self, ban_id: u64) -> Result<()>;

    // ---- complaints (§10.4) ----

    async fn complaints(&self) -> Result<Vec<Row>>;
    async fn add_complaint(&self, target_db: &DbId, message: &str) -> Result<()>;
    /// Remove the complaint against `target_db` that was filed by
    /// `filed_by_db` (both come straight from the `complainlist` row).
    async fn delete_complaint(&self, target_db: u64, filed_by_db: u64) -> Result<()>;

    // ---- runtime self state (§11) ----

    async fn update_self(&self, update: SelfUpdate) -> Result<()>;

    // ---- whisper (§6.5) ----

    /// Whisper one Opus frame (20 ms) to every client inside `channel`.
    /// Verified on server 3.13.8 with the new whisper protocol:
    /// whisper_type=1, target=0, target_id=channel id. Client-targeted
    /// whispering needs the whisper-list machinery (not yet implemented).
    async fn send_whisper_to_channel(&self, channel: &univox_core::id::ChannelId, frame: &[u8]) -> Result<()>;

    // ---- plugin command relay (§11) ----

    /// `plugincmd` — relay a plugin message to other clients. The wire
    /// targetmode is the official PluginTargetMode value (Single=0,
    /// CurrentTab=1, Clients=2, All=3); Clients/Single additionally need
    /// `target_client`. On server 3.13.8 CurrentTab and All are accepted.
    async fn send_plugin_command(
        &self,
        target: PluginCommandTarget,
        name: &str,
        data: &str,
        target_client: Option<&MemberId>,
    ) -> Result<()>;

    // ---- local muting (§11) ----

    /// Locally mute members (server is not involved in the audio path —
    /// this only stops our own client from decoding their voice).
    async fn mute_members(&self, members: &[MemberId]) -> Result<()>;
    async fn unmute_members(&self, members: &[MemberId]) -> Result<()>;

    // ---- server snapshot (§8.5) ----

    async fn server_snapshot(&self) -> Result<String>;
    async fn deploy_server_snapshot(&self, snapshot: &str) -> Result<()>;
}

/// `list`-style commands answer 1281 "database empty result set" when
/// there is nothing to list — treat that as an empty result.
fn rows_or_empty(r: Result<Vec<Row>>) -> Result<Vec<Row>> {
    match r {
        Ok(rows) => Ok(rows),
        Err(Error::Platform { code: 1281, .. }) => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}

fn member_from_row(row: &Row, id_key: &str) -> ClientMatch {
    ClientMatch {
        member: MemberId::from_u64(row.get(id_key).and_then(|v| v.parse().ok()).unwrap_or(0)),
        uid: row.get("client_unique_identifier").unwrap_or("").to_string(),
        nickname: row.get("client_nickname").unwrap_or("").to_string(),
    }
}

fn db_entry_from_row(row: &Row) -> ClientDbEntry {
    ClientDbEntry {
        db_id: DbId::from_u64(row.get("cldbid").and_then(|v| v.parse().ok()).unwrap_or(0)),
        uid: row.get("client_unique_identifier").unwrap_or("").to_string(),
        nickname: row.get("client_nickname").unwrap_or("").to_string(),
        description: row.get("client_description").unwrap_or("").to_string(),
        last_connected: row.get("client_lastconnected").and_then(|v| v.parse().ok()),
        total_connections: row
            .get("client_totalconnections")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
    }
}

fn message_from_row(row: &Row) -> Message {
    Message {
        id: MessageId::from_string(
            row.get("msgid").unwrap_or("0").to_string(),
        ),
        author_name: row.get("cluid").unwrap_or("").to_string(),
        content: row.get("message").unwrap_or("").to_string(),
        extra: [
            ("subject".to_string(), row.get("subject").unwrap_or("").to_string()),
            (
                "timestamp".to_string(),
                row.get("timestamp").unwrap_or("").to_string(),
            ),
            (
                "read".to_string(),
                row.get("flag_read").unwrap_or("").to_string(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

#[async_trait]
impl Ts3Ext for Ts3Session {
    async fn uid_from_clid(&self, member: &MemberId) -> Result<String> {
        let rows = self
            .exec(
                Command::new("clientgetuidfromclid")
                    .param("clid", member.as_u64().unwrap_or(0)),
            )
            .await?;
        rows.first()
            .and_then(|r| r.get("cluid"))
            .map(String::from)
            .ok_or_else(|| Error::Other("clientgetuidfromclid: no cluid".into()))
    }

    async fn dbid_from_uid(&self, uid: &str) -> Result<Option<DbId>> {
        let rows = self
            .exec(Command::new("clientgetdbidfromuid").param("cluid", uid))
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("cldbid"))
            .and_then(|v| v.parse().ok())
            .map(DbId::from_u64))
    }

    async fn name_from_uid(&self, uid: &str) -> Result<Option<String>> {
        let rows = self
            .exec(Command::new("clientgetnamefromuid").param("cluid", uid))
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("name"))
            .map(String::from))
    }

    async fn name_from_dbid(&self, db_id: &DbId) -> Result<Option<String>> {
        let rows = self
            .exec(Command::new("clientgetnamefromdbid").param("cldbid", db_id.as_u64().unwrap_or(0)))
            .await?;
        Ok(rows
            .first()
            .and_then(|r| r.get("name"))
            .map(String::from))
    }

    async fn find_clients(&self, pattern: &str) -> Result<Vec<ClientMatch>> {
        let rows = self
            .exec(Command::new("clientfind").param("pattern", pattern))
            .await?;
        Ok(rows.iter().map(|r| member_from_row(r, "clid")).collect())
    }

    async fn client_db_list(&self, start: u64, limit: u64) -> Result<Vec<ClientDbEntry>> {
        let rows = rows_or_empty(
            self.exec(
                Command::new("clientdblist")
                    .param("start", start)
                    .param("duration", limit),
            )
            .await,
        )?;
        Ok(rows.iter().map(db_entry_from_row).collect())
    }

    async fn client_db_info(&self, db_id: &DbId) -> Result<Row> {
        let rows = self
            .exec(Command::new("clientdbinfo").param("cldbid", db_id.as_u64().unwrap_or(0)))
            .await?;
        rows.first().cloned().ok_or_else(|| Error::Other("clientdbinfo: no row".into()))
    }

    async fn client_db_edit(&self, db_id: &DbId, description: &str) -> Result<()> {
        self.exec_ok(
            Command::new("clientdbedit")
                .param("cldbid", db_id.as_u64().unwrap_or(0))
                .param("client_description", description),
        )
        .await
    }

    async fn client_db_delete(&self, db_id: &DbId) -> Result<()> {
        self.exec_ok(Command::new("clientdbdelete").param("cldbid", db_id.as_u64().unwrap_or(0)))
            .await
    }

    async fn custom_info(&self, db_id: &DbId) -> Result<Vec<(String, String)>> {
        let rows = rows_or_empty(
            self.exec(Command::new("custominfo").param("cldbid", db_id.as_u64().unwrap_or(0)))
                .await,
        )?;
        Ok(rows
            .first()
            .map(|r| {
                r.iter()
                    .filter(|(k, _)| k.starts_with("custom_"))
                    .map(|(k, v)| (k.trim_start_matches("custom_").to_string(), v.clone()))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn custom_search(&self, key: &str, value: &str) -> Result<Vec<Row>> {
        self.exec(
            Command::new("customsearch")
                .param("ident", key)
                .param("pattern", value),
        )
        .await
    }

    async fn offline_messages(&self) -> Result<Vec<Message>> {
        let rows = rows_or_empty(self.exec(Command::new("messagelist")).await)?;
        Ok(rows.iter().map(message_from_row).collect())
    }

    async fn send_offline_message(&self, to_uid: &str, subject: &str, text: &str) -> Result<()> {
        self.exec_ok(
            Command::new("messageadd")
                .param("cluid", to_uid)
                .param("subject", subject)
                .param("message", text),
        )
        .await
    }

    async fn offline_message(&self, id: &MessageId) -> Result<Message> {
        let rows = self
            .exec(Command::new("messageget").param("msgid", id.as_str().parse::<u64>().unwrap_or(0)))
            .await?;
        rows.first()
            .map(message_from_row)
            .ok_or_else(|| Error::Other("messageget: no row".into()))
    }

    async fn delete_offline_message(&self, id: &MessageId) -> Result<()> {
        self.exec_ok(Command::new("messagedel").param("msgid", id.as_str().parse::<u64>().unwrap_or(0)))
            .await
    }

    async fn bans(&self) -> Result<Vec<Row>> {
        rows_or_empty(self.exec(Command::new("banlist")).await)
    }

    async fn add_ban(
        &self,
        ip: Option<&str>,
        name: Option<&str>,
        uid: Option<&str>,
        duration: Option<Duration>,
        reason: Option<&str>,
    ) -> Result<()> {
        if ip.is_none() && name.is_none() && uid.is_none() {
            return Err(Error::InvalidArgument(
                "add_ban needs at least one of ip/name/uid".into(),
            ));
        }
        let mut cmd = Command::new("banadd");
        if let Some(v) = ip {
            cmd = cmd.param("ip", v);
        }
        if let Some(v) = name {
            cmd = cmd.param("name", v);
        }
        if let Some(v) = uid {
            cmd = cmd.param("uid", v);
        }
        if let Some(d) = duration {
            cmd = cmd.param("time", d.as_secs());
        }
        if let Some(r) = reason {
            cmd = cmd.param("banreason", r);
        }
        self.exec_ok(cmd).await
    }

    async fn remove_ban(&self, ban_id: u64) -> Result<()> {
        self.exec_ok(Command::new("bandel").param("banid", ban_id))
            .await
    }

    async fn complaints(&self) -> Result<Vec<Row>> {
        rows_or_empty(self.exec(Command::new("complainlist")).await)
    }

    async fn add_complaint(&self, target_db: &DbId, message: &str) -> Result<()> {
        self.exec_ok(
            Command::new("complainadd")
                .param("tcldbid", target_db.as_u64().unwrap_or(0))
                .param("message", message),
        )
        .await
    }

    async fn delete_complaint(&self, target_db: u64, filed_by_db: u64) -> Result<()> {
        self.exec_ok(
            Command::new("complaindel")
                .param("tcldbid", target_db)
                .param("fcldbid", filed_by_db),
        )
        .await
    }

    async fn update_self(&self, update: SelfUpdate) -> Result<()> {
        self.exec_ok(update.into_command()).await
    }

    async fn send_whisper_to_channel(
        &self,
        channel: &univox_core::id::ChannelId,
        frame: &[u8],
    ) -> Result<()> {
        // New whisper format: [codec][whisper_type][target][target_id:8]
        // [opus data]; whisper_type 1 targets a channel. The connection
        // prepends the voice sequence id.
        let mut content = Vec::with_capacity(11 + frame.len());
        content.push(univox_voice::CODEC_OPUS_VOICE);
        content.push(1); // whisper_type: channel
        content.push(0); // target
        content.extend_from_slice(&channel.as_u64().unwrap_or(0).to_be_bytes());
        content.extend_from_slice(frame);
        self.conn().send_voice(content, univox_ts3_proto::PacketType::VoiceWhisper).await;
        Ok(())
    }

    async fn send_plugin_command(
        &self,
        target: PluginCommandTarget,
        name: &str,
        data: &str,
        target_client: Option<&MemberId>,
    ) -> Result<()> {
        let targetmode = match target {
            PluginCommandTarget::Single => 0u8,
            PluginCommandTarget::CurrentTab => 1,
            PluginCommandTarget::Clients => 2,
            PluginCommandTarget::All => 3,
        };
        let mut cmd = Command::new("plugincmd")
            .param("targetmode", targetmode)
            .param("name", name)
            .param("data", data);
        if let Some(m) = target_client {
            cmd = cmd.param("target", m.as_u64().unwrap_or(0));
        }
        self.exec_ok(cmd).await
    }

    async fn mute_members(&self, members: &[MemberId]) -> Result<()> {
        let mut cmd = Command::new("clientmute");
        for m in members {
            cmd = cmd.param("clid", m.as_u64().unwrap_or(0));
        }
        self.exec_ok(cmd).await
    }

    async fn unmute_members(&self, members: &[MemberId]) -> Result<()> {
        let mut cmd = Command::new("clientunmute");
        for m in members {
            cmd = cmd.param("clid", m.as_u64().unwrap_or(0));
        }
        self.exec_ok(cmd).await
    }

    async fn server_snapshot(&self) -> Result<String> {
        let rows = self.exec(Command::new("serversnapshotcreate")).await?;
        rows.first()
            .map(|r| {
                r.iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .ok_or_else(|| Error::Other("serversnapshotcreate: empty".into()))
    }

    async fn deploy_server_snapshot(&self, snapshot: &str) -> Result<()> {
        let mut cmd = Command::new("serversnapshotdeploy");
        for pair in snapshot.split_whitespace() {
            if let Some((k, v)) = pair.split_once('=') {
                cmd = cmd.param(k, v);
            }
        }
        self.exec_ok(cmd).await
    }
}
