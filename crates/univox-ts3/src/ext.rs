//! TS3 platform extension layer (FEATURES.md §9/§10/§11): the client-side
//! capabilities that have no cross-platform equivalent — ID mapping,
//! client database, offline messages, bans/complaints, runtime self state,
//! plugin commands and local muting.

use std::time::Duration;

use async_trait::async_trait;

use univox_core::error::{Error, Result};
use univox_core::event::PluginCommandTarget;
use univox_core::id::{DbId, MemberId, MessageId, RoleId};
use univox_core::model::Message;
use univox_core::session::Session;
use univox_ts3_proto::{Command, Row, RowExt, hash_password};

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

/// A server group (`servergrouplist`). Server groups are univox roles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerGroup {
    pub id: RoleId,
    pub name: String,
    /// `sgtype`: 1 = template, 2 = regular, 3 = ServerQuery.
    pub kind: u8,
}

/// A channel group (`channelgrouplist`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelGroup {
    pub id: u64,
    pub name: String,
    /// `cgtype`: 1 = template, 2 = regular, 3 = ServerQuery.
    pub kind: u8,
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
    pub(crate) fn into_command(self) -> Command {
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

/// One whisper destination (FEATURES.md §6.4): either every client inside
/// a channel or a single member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WhisperTarget {
    Member(MemberId),
    Channel(univox_core::id::ChannelId),
}

/// The server's hard cap on the number of whisper destinations per packet.
pub const WHISPER_MAX_TARGETS: usize = 65;

/// A locally stored whisper list — named target set, activatable for
/// one-shot sending (FEATURES.md §6.4). Purely client-side state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WhisperList {
    pub id: u64,
    pub name: String,
    pub targets: Vec<WhisperTarget>,
}

/// In-memory whisper-list storage of a session (crate-internal).
#[derive(Default)]
pub struct WhisperListState {
    pub(crate) lists: Vec<WhisperList>,
    pub(crate) active: Option<u64>,
    pub(crate) next_id: u64,
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

    // ---- whisper (§6.4) ----

    /// Whisper one Opus frame (20 ms) to a mixed list of members and
    /// channels (at most 65 targets — the server's hard limit). The audio
    /// travels as voice-whisper packets and does not reach anyone else,
    /// including the clients inside our own channel.
    ///
    /// Note that the server silently drops the audio when the sender's
    /// `i_client_whisper_power` or a target's `i_client_needed_whisper_power`
    /// gate it — voice packets are not acknowledged, so `Ok(())` only means
    /// "sent", not "heard".
    async fn send_whisper(&self, targets: &[WhisperTarget], frame: &[u8]) -> Result<()>;
    /// Whisper one Opus frame (20 ms) to every client inside `channel`.
    /// Verified on server 3.13.8 with the new whisper protocol:
    /// whisper_type=1, target=0, target_id=channel id.
    async fn send_whisper_to_channel(&self, channel: &univox_core::id::ChannelId, frame: &[u8]) -> Result<()>;

    /// The locally stored whisper lists (§6.4: a purely client-side
    /// concept — the original client keeps them in its config, the server
    /// never sees them). The list survives reconnects but not process
    /// restarts.
    fn whisper_lists(&self) -> Vec<WhisperList>;
    /// Add a whisper list; returns its id (also usable as the activation
    /// handle for [`Ts3Ext::set_active_whisper_list`]).
    async fn add_whisper_list(&self, name: &str, targets: Vec<WhisperTarget>) -> Result<u64>;
    /// Remove a whisper list; deactivates it when it was active.
    async fn remove_whisper_list(&self, id: u64) -> Result<()>;
    /// Activate a whisper list (or none). The active list is what
    /// [`Ts3Ext::send_whisper_to_active_list`] sends to.
    async fn set_active_whisper_list(&self, id: Option<u64>) -> Result<()>;
    /// The id of the currently active whisper list, if any.
    async fn active_whisper_list(&self) -> Option<u64>;
    /// Whisper one Opus frame to every target of the active whisper list.
    async fn send_whisper_to_active_list(&self, frame: &[u8]) -> Result<()>;

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

    // ---- file transfer (§11) ----

    /// List the files of a channel's root (or `path` subdirectory).
    /// `password` is the channel password, plaintext — hashed internally.
    async fn list_files(
        &self,
        channel: &univox_core::id::ChannelId,
        path: &str,
        password: Option<&str>,
    ) -> Result<Vec<Row>>;
    /// Upload `data` as `name` (e.g. `/univox_test.txt`) into `channel`.
    async fn upload_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        data: &[u8],
        overwrite: bool,
    ) -> Result<()>;
    /// Download the file `name` from `channel`.
    async fn download_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
    ) -> Result<Vec<u8>>;
    /// Stream-download `name` from `channel` in chunks (up to 16 KiB);
    /// see [`FileDownload`](crate::filetransfer::FileDownload) for
    /// progress (`size`/`received`) and cancellation (drop = finalize).
    /// `password` is the channel password, plaintext — hashed internally.
    async fn download_file_stream(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        password: Option<&str>,
    ) -> Result<crate::filetransfer::FileDownload>;
    /// Stream-upload exactly `size` bytes as `name` into `channel`
    /// (overwrites an existing file); see
    /// [`FileUpload`](crate::filetransfer::FileUpload). Dropping the
    /// handle before `finish` aborts and deletes the partial file.
    /// `password` is the channel password, plaintext — hashed internally.
    async fn upload_file_stream(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        size: u64,
        password: Option<&str>,
    ) -> Result<crate::filetransfer::FileUpload>;
    /// `ftcreatedir` — create a directory in `channel`'s file area
    /// (`path` like `/newdir` or `/parent/newdir`). `password` is the
    /// channel password, plaintext — hashed internally.
    async fn create_dir(
        &self,
        channel: &univox_core::id::ChannelId,
        path: &str,
        password: Option<&str>,
    ) -> Result<()>;
    /// Delete the file `name` from `channel` (also removes empty
    /// directories). `password` is the channel password, plaintext —
    /// hashed internally.
    async fn delete_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        password: Option<&str>,
    ) -> Result<()>;
    /// The client's avatar bytes, if one is set (None = no avatar).
    async fn download_avatar(&self, db_id: &DbId) -> Result<Option<Vec<u8>>>;
    /// The avatar of the client with unique id `uid` (resolves the
    /// database id internally), if one is set.
    async fn download_avatar_by_uid(&self, uid: &str) -> Result<Option<Vec<u8>>>;
    /// Set this client's avatar to `data`.
    async fn upload_avatar(&self, data: &[u8]) -> Result<()>;

    // ---- channel administration (§8.4) ----

    /// Re-parent a channel and place it among the new parent's children
    /// (`channelmove`). `order` is the sibling id to sort the channel
    /// after (0 = first). `password` is the moved channel's own password,
    /// plaintext — hashed internally like every channel password.
    async fn move_channel(
        &self,
        channel: &univox_core::id::ChannelId,
        parent: &univox_core::id::ChannelId,
        order: u64,
        password: Option<&str>,
    ) -> Result<()>;

    // ---- typed queries (§8.3/§9.6) ----

    /// `servergrouplist`. Server groups are univox roles.
    async fn server_groups(&self) -> Result<Vec<ServerGroup>>;
    /// `channelgrouplist`.
    async fn channel_groups(&self) -> Result<Vec<ChannelGroup>>;
    /// `clientpermlist` for this client's own database id, as
    /// `(permission name or numeric id, value)` pairs.
    async fn own_permissions(&self) -> Result<Vec<(String, i64)>>;
    /// `channelsubscribeall` — subscribe to every channel on the server.
    async fn subscribe_all(&self) -> Result<()>;
}

/// The file-transfer path of a client's avatar: `/avatar_<hash>`, where
/// `hash` is the server's `client_base64HashClientUID` (see
/// `client_db_info`). Useful when transferring avatars by hand via
/// [`Ts3Ext::download_file_stream`].
pub fn avatar_path(hash: &str) -> String {
    format!("/avatar_{hash}")
}

/// Wait for a transfer-status notification (`notifystartupload`,
/// `notifystartdownload` or `notifystatusfiletransfer`) whose clientftfid
/// matches ours (up to 3 s). The ft negotiation answers AFTER the error
/// packet via these notifications.
async fn wait_for_start(
    notifications: &mut crate::client::NotificationStream,
    names: &[&str],
    clientftfid: u32,
) -> Result<Row> {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_millis(500), notifications.recv())
            .await
        {
            Ok(Some(cmd)) if names.contains(&cmd.name.as_str()) => {
                let row = cmd.params.first().cloned().unwrap_or_default();
                if row.get("clientftfid").map(|v| v == clientftfid.to_string()) == Some(true) {
                    return Ok(row);
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    Err(Error::Timeout)
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

/// Build the voice content of a legacy-format whisper packet
/// (Newprotocol unset, tsdeclarations §1.8.2.1): after the codec byte come
/// the channel count, the client count, then the channel ids (u64 BE) and
/// the client ids (u16 BE), then the opus frame. The connection prepends
/// the voice sequence id.
pub(crate) fn build_whisper_packet(targets: &[WhisperTarget], frame: &[u8]) -> Result<Vec<u8>> {
    let (channels, members): (Vec<&WhisperTarget>, Vec<&WhisperTarget>) =
        targets.iter().partition(|t| matches!(t, WhisperTarget::Channel(_)));
    let mut content =
        Vec::with_capacity(3 + channels.len() * 8 + members.len() * 2 + frame.len());
    content.push(univox_ts3_proto::CODEC_OPUS_VOICE);
    content.push(channels.len() as u8);
    content.push(members.len() as u8);
    for t in channels {
        let WhisperTarget::Channel(c) = t else { unreachable!("partitioned") };
        content.extend_from_slice(&c.as_u64().unwrap_or(0).to_be_bytes());
    }
    for t in members {
        let WhisperTarget::Member(m) = t else { unreachable!("partitioned") };
        content.extend_from_slice(&(m.as_u64().unwrap_or(0) as u16).to_be_bytes());
    }
    content.extend_from_slice(frame);
    Ok(content)
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
        // Remember for the reconnect replay (supervisor, restore_state).
        *self.last_self_update.lock().unwrap() = Some(update.clone());
        self.exec_ok(update.into_command()).await
    }

    async fn send_whisper(&self, targets: &[WhisperTarget], frame: &[u8]) -> Result<()> {
        if targets.is_empty() {
            return Err(Error::InvalidArgument("send_whisper: no targets".into()));
        }
        if targets.len() > WHISPER_MAX_TARGETS {
            return Err(Error::InvalidArgument(format!(
                "send_whisper: {} targets exceed the server limit of {WHISPER_MAX_TARGETS}",
                targets.len()
            )));
        }
        let content = build_whisper_packet(targets, frame)?;
        self.conn().send_voice(content, univox_ts3_proto::PacketType::VoiceWhisper).await;
        Ok(())
    }

    async fn send_whisper_to_channel(
        &self,
        channel: &univox_core::id::ChannelId,
        frame: &[u8],
    ) -> Result<()> {
        // Verified on server 3.13.8 with the new whisper protocol:
        // [codec][whisper_type=1][target=0][cid:8][opus]; whisper_type 1
        // targets a channel.
        let mut content = Vec::with_capacity(11 + frame.len());
        content.push(univox_ts3_proto::CODEC_OPUS_VOICE);
        content.push(1); // whisper_type: channel
        content.push(0); // target
        content.extend_from_slice(&channel.as_u64().unwrap_or(0).to_be_bytes());
        content.extend_from_slice(frame);
        self.conn().send_voice(content, univox_ts3_proto::PacketType::VoiceWhisper).await;
        Ok(())
    }

    fn whisper_lists(&self) -> Vec<WhisperList> {
        let st = self.whisper_lists.lock().unwrap();
        st.lists.clone()
    }

    async fn add_whisper_list(&self, name: &str, targets: Vec<WhisperTarget>) -> Result<u64> {
        if targets.is_empty() {
            return Err(Error::InvalidArgument("add_whisper_list: no targets".into()));
        }
        if targets.len() > WHISPER_MAX_TARGETS {
            return Err(Error::InvalidArgument(format!(
                "add_whisper_list: {} targets exceed the server limit of {WHISPER_MAX_TARGETS}",
                targets.len()
            )));
        }
        let mut st = self.whisper_lists.lock().unwrap();
        let id = st.next_id;
        st.next_id += 1;
        st.lists.push(WhisperList { id, name: name.to_owned(), targets });
        Ok(id)
    }

    async fn remove_whisper_list(&self, id: u64) -> Result<()> {
        let mut st = self.whisper_lists.lock().unwrap();
        let before = st.lists.len();
        st.lists.retain(|l| l.id != id);
        if st.active == Some(id) {
            st.active = None;
        }
        if st.lists.len() == before {
            return Err(Error::Other(format!("no whisper list with id {id}")));
        }
        Ok(())
    }

    async fn set_active_whisper_list(&self, id: Option<u64>) -> Result<()> {
        let mut st = self.whisper_lists.lock().unwrap();
        if let Some(id) = id {
            if !st.lists.iter().any(|l| l.id == id) {
                return Err(Error::Other(format!("no whisper list with id {id}")));
            }
        }
        st.active = id;
        Ok(())
    }

    async fn active_whisper_list(&self) -> Option<u64> {
        self.whisper_lists.lock().unwrap().active
    }

    async fn send_whisper_to_active_list(&self, frame: &[u8]) -> Result<()> {
        let active = self.whisper_lists.lock().unwrap().active;
        let list = active
            .and_then(|id| self.whisper_lists.lock().unwrap().lists.iter().find(|l| l.id == id).cloned())
            .ok_or_else(|| Error::Other("no active whisper list".into()))?;
        self.send_whisper(&list.targets, frame).await
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

    async fn list_files(
        &self,
        channel: &univox_core::id::ChannelId,
        path: &str,
        password: Option<&str>,
    ) -> Result<Vec<Row>> {
        // This server requires `cpw` (empty) along with path, and answers
        // AFTER the error packet with `notifyfilelist` rows terminated by
        // `notifyfilelistfinished`.
        let cpw = password.map(hash_password).unwrap_or_default();
        let mut notifications = self.conn().subscribe();
        if let Err(Error::Platform { code: 1281, .. } | Error::Platform { code: 2054, .. }) = self
            .exec(
                Command::new("ftgetfilelist")
                    .param("cid", channel.as_u64().unwrap_or(0))
                    .param("cpw", cpw)
                    .param("path", path),
            )
            .await
        {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(
                std::time::Duration::from_millis(500),
                notifications.recv(),
            )
            .await
            {
                Ok(Some(cmd)) if cmd.name == "notifyfilelist" => out.extend(cmd.params),
                Ok(Some(cmd)) if cmd.name == "notifyfilelistfinished" => break,
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        Ok(out)
    }

    async fn upload_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        data: &[u8],
        overwrite: bool,
    ) -> Result<()> {
        let mut up = self
            .start_upload(channel, name, data.len() as u64, overwrite, None)
            .await?;
        up.write_chunk(data).await?;
        up.finish().await
    }

    async fn download_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
    ) -> Result<Vec<u8>> {
        let mut dl = self.download_file_stream(channel, name, None).await?;
        let mut out = Vec::with_capacity(dl.size() as usize);
        while let Some(chunk) = dl.next_chunk().await? {
            out.extend_from_slice(&chunk);
        }
        Ok(out)
    }

    async fn download_file_stream(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        password: Option<&str>,
    ) -> Result<crate::filetransfer::FileDownload> {
        let cpw = password.map(hash_password).unwrap_or_default();
        let row = self.ft_init_download(channel, name, 0, &cpw).await?;
        let serverftfid: u32 =
            row.get("serverftfid").and_then(|v| v.parse().ok()).unwrap_or(0);
        let size: u64 = row.get("size").and_then(|v| v.parse().ok()).unwrap_or(0);
        let ft = transfer_channel_from(&row, self.conn().addr).await?;
        Ok(crate::filetransfer::FileDownload::new(
            ft,
            std::sync::Arc::downgrade(&self.conn()),
            serverftfid,
            size,
        ))
    }

    async fn upload_file_stream(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        size: u64,
        password: Option<&str>,
    ) -> Result<crate::filetransfer::FileUpload> {
        self.start_upload(channel, name, size, true, password).await
    }

    async fn create_dir(
        &self,
        channel: &univox_core::id::ChannelId,
        path: &str,
        password: Option<&str>,
    ) -> Result<()> {
        let cpw = password.map(hash_password).unwrap_or_default();
        self.exec_ok(
            Command::new("ftcreatedir")
                .param("cid", channel.as_u64().unwrap_or(0))
                .param("dirname", path)
                .param("cpw", cpw),
        )
        .await
    }

    async fn delete_file(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        password: Option<&str>,
    ) -> Result<()> {
        let cpw = password.map(hash_password).unwrap_or_default();
        self.exec_ok(
            Command::new("ftdeletefile")
                .param("cid", channel.as_u64().unwrap_or(0))
                .param("cpw", cpw)
                .param("name", name),
        )
        .await
    }

    async fn download_avatar(&self, db_id: &DbId) -> Result<Option<Vec<u8>>> {
        // Avatars live on the server root under /avatar_<hash>, where hash
        // is the server-computed `client_base64HashClientUID`.
        let info = self.client_db_info(db_id).await?;
        let Some(hash) = info
            .get("client_base64HashClientUID")
            .filter(|v| !v.is_empty())
            .map(String::from)
        else {
            return Ok(None);
        };
        let path = avatar_path(&hash);
        match self
            .download_file(&univox_core::id::ChannelId::from_u64(0), &path)
            .await
        {
            Ok(data) => Ok(Some(data)),
            // 2054 = invalid file path: no avatar stored for this client.
            Err(Error::Platform { code, .. }) if matches!(code, 1281 | 2054) => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn download_avatar_by_uid(&self, uid: &str) -> Result<Option<Vec<u8>>> {
        match self.dbid_from_uid(uid).await? {
            Some(db) => self.download_avatar(&db).await,
            None => Ok(None),
        }
    }

    async fn upload_avatar(&self, data: &[u8]) -> Result<()> {
        // The server stores the upload under /avatar_<our hash>; the name
        // we upload under is irrelevant.
        let hash = self
            .client_db_info(&self.own_dbid().await?)
            .await?
            .get("client_base64HashClientUID")
            .map(String::from)
            .unwrap_or_default();
        let path = avatar_path(&hash);
        self.upload_file(&univox_core::id::ChannelId::from_u64(0), &path, data, true)
            .await?;
        // Mark the member row so everyone sees the new avatar.
        self.exec_ok(
            Command::new("clientupdate").param("client_flag_avatar", md5_hex(data)),
        )
        .await
    }

    async fn move_channel(
        &self,
        channel: &univox_core::id::ChannelId,
        parent: &univox_core::id::ChannelId,
        order: u64,
        password: Option<&str>,
    ) -> Result<()> {
        // TS3 quirk (verified on 3.13.8): reordering within the SAME parent
        // via `channelmove` errors 770 ("already member of channel") — that
        // goes through `channeledit` + `channel_order` instead. A parent
        // change uses `channelmove`.
        let current = self
            .book()
            .channel(channel)
            .and_then(|c| c.parent_id)
            .and_then(|p| p.as_u64());
        if current.is_some() && current == parent.as_u64() {
            return self
                .exec_ok(
                    Command::new("channeledit")
                        .param("cid", channel.as_u64().unwrap_or(0))
                        .param("channel_order", order),
                )
                .await;
        }
        let mut cmd = Command::new("channelmove")
            .param("cid", channel.as_u64().unwrap_or(0))
            .param("cpid", parent.as_u64().unwrap_or(0))
            .param("order", order);
        if let Some(pw) = password {
            cmd = cmd.param("cpw", hash_password(pw));
        }
        self.exec_ok(cmd).await
    }

    async fn server_groups(&self) -> Result<Vec<ServerGroup>> {
        let rows = self
            .exec_list(
                Command::new("servergrouplist"),
                "notifyservergrouplist",
                "notifyservergrouplistfinished",
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| ServerGroup {
                id: RoleId::from_u64(r.get("sgid").and_then(|v| v.parse().ok()).unwrap_or(0)),
                name: r.get("name").unwrap_or_default().to_string(),
                kind: r.get("sgtype").and_then(|v| v.parse().ok()).unwrap_or(0),
            })
            .collect())
    }

    async fn channel_groups(&self) -> Result<Vec<ChannelGroup>> {
        let rows = self
            .exec_list(
                Command::new("channelgrouplist"),
                "notifychannelgrouplist",
                "notifychannelgrouplistfinished",
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|r| ChannelGroup {
                id: r.get("cgid").and_then(|v| v.parse().ok()).unwrap_or(0),
                name: r.get("name").unwrap_or_default().to_string(),
                kind: r.get("cgtype").and_then(|v| v.parse().ok()).unwrap_or(0),
            })
            .collect())
    }

    async fn own_permissions(&self) -> Result<Vec<(String, i64)>> {
        let db = self.own_dbid().await?;
        let rows = match self
            .exec_list(
                Command::new("clientpermlist")
                    .param("cldbid", db.as_u64().unwrap_or(0))
                    .opt("permsid"),
                "notifyclientpermlist",
                "notifyclientpermlistfinished",
            )
            .await
        {
            Ok(rows) => rows,
            // 1281 "database empty result set": the client has no
            // permissions stored — an empty list, not an error.
            Err(Error::Platform { code: 1281, .. }) => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(rows
            .into_iter()
            .map(|r| {
                let name = r
                    .get("permsid")
                    .map(String::from)
                    .or_else(|| r.get("permid").map(String::from))
                    .unwrap_or_default();
                let value = r
                    .get("permvalue")
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);
                (name, value)
            })
            .collect())
    }

    async fn subscribe_all(&self) -> Result<()> {
        self.exec_ok(Command::new("channelsubscribeall")).await
    }
}

impl Ts3Session {
    /// This session's own client database id.
    pub(crate) async fn own_dbid(&self) -> Result<DbId> {
        let uid = self.own_uid().await?;
        self.dbid_from_uid(&uid)
            .await?
            .ok_or_else(|| Error::Other("own dbid unknown".into()))
    }

    /// Negotiate an upload (`ftinitupload`) and open the payload channel.
    async fn start_upload(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        size: u64,
        overwrite: bool,
        password: Option<&str>,
    ) -> Result<crate::filetransfer::FileUpload> {
        let cpw = password.map(hash_password).unwrap_or_default();
        let row = self.ft_init_upload(channel, name, size, overwrite, &cpw).await?;
        let serverftfid: u32 =
            row.get("serverftfid").and_then(|v| v.parse().ok()).unwrap_or(0);
        let ft = transfer_channel_from(&row, self.conn().addr).await?;
        Ok(crate::filetransfer::FileUpload::new(
            ft,
            std::sync::Arc::downgrade(&self.conn()),
            serverftfid,
            size,
        ))
    }

    /// Send `ftinitupload` and wait for the transfer parameters.
    async fn ft_init_upload(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        size: u64,
        overwrite: bool,
        cpw: &str,
    ) -> Result<Row> {
        let clientftfid = self.next_ftfid();
        // The transfer parameters arrive as `notifystartupload` AFTER the
        // command's error packet — subscribe first so nothing is missed.
        let mut notifications = self.conn().subscribe();
        self.exec(
            Command::new("ftinitupload")
                .param("clientftfid", clientftfid)
                .param("serverftfid", 0)
                .param("cid", channel.as_u64().unwrap_or(0))
                .param("name", name)
                .param("cpw", cpw)
                .param("size", size)
                .param("overwrite", u8::from(overwrite))
                .param("resume", 0)
                .param("proto", 1),
        )
        .await?;
        let row = wait_for_start(
            &mut notifications,
            &["notifystartupload", "notifystatusfiletransfer"],
            clientftfid,
        )
        .await?;
        check_ft_status(&row, "upload failed")?;
        Ok(row)
    }

    /// Send `ftinitdownload` and wait for the transfer parameters.
    async fn ft_init_download(
        &self,
        channel: &univox_core::id::ChannelId,
        name: &str,
        seekpos: u64,
        cpw: &str,
    ) -> Result<Row> {
        let clientftfid = self.next_ftfid();
        let mut notifications = self.conn().subscribe();
        self.exec(
            Command::new("ftinitdownload")
                .param("clientftfid", clientftfid)
                .param("name", name)
                .param("cid", channel.as_u64().unwrap_or(0))
                .param("cpw", cpw)
                .param("seekpos", seekpos)
                .param("proto", 1),
        )
        .await?;
        let row = wait_for_start(
            &mut notifications,
            &["notifystartdownload", "notifystatusfiletransfer"],
            clientftfid,
        )
        .await?;
        check_ft_status(&row, "download failed")?;
        Ok(row)
    }

    /// Run a list command whose rows stream in as notifications after the
    /// error packet (`servergrouplist`, `clientpermlist`, ...). Rows
    /// returned inline with the command are used directly; notification
    /// rows are appended until the `finished` marker or a quiet gap.
    pub(crate) async fn exec_list(
        &self,
        cmd: Command,
        notify_name: &str,
        finished: &str,
    ) -> Result<Vec<Row>> {
        let mut notifications = self.conn().subscribe();
        let mut rows = self.exec(cmd).await?;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            match tokio::time::timeout(
                std::time::Duration::from_millis(300),
                notifications.recv(),
            )
            .await
            {
                Ok(Some(c)) if c.name == notify_name => rows.extend(c.params),
                Ok(Some(c)) if c.name == finished => break,
                Ok(Some(_)) => {}
                // 300 ms without a follow-up: the list is complete.
                Ok(None) | Err(_) => break,
            }
        }
        Ok(rows)
    }
}

/// Surface a nonzero transfer `status` as a platform error.
fn check_ft_status(row: &Row, what: &str) -> Result<()> {
    if let Some(status) = row.get("status").and_then(|v| v.parse::<i64>().ok()) {
        if status != 0 {
            return Err(Error::Platform {
                platform: univox_core::platform::Platform::Ts3,
                code: status as i32,
                message: row.get("msg").unwrap_or(what).to_string(),
            });
        }
    }
    Ok(())
}

/// The IP to open the payload channel against. Servers that bind the file
/// transfer port to every interface (the default `filetransfer_ip=0.0.0.0`)
/// report `ip=0.0.0.0` or no usable ip at all — like the official client,
/// fall back to the voice connection's peer address then.
fn transfer_ip(reported: Option<&str>, voice_peer: std::net::SocketAddr) -> String {
    match reported.and_then(|ip| ip.parse::<std::net::IpAddr>().ok()) {
        Some(ip) if !ip.is_unspecified() => ip.to_string(),
        _ => voice_peer.ip().to_string(),
    }
}

/// Open the payload TCP channel from a transfer-start row.
async fn transfer_channel_from(
    row: &Row,
    voice_peer: std::net::SocketAddr,
) -> Result<crate::filetransfer::TransferChannel> {
    let port: u16 = row.get("port").and_then(|v| v.parse().ok()).unwrap_or(0);
    let ip = transfer_ip(row.get("ip"), voice_peer);
    tracing::debug!(
        reported = ?row.get("ip"),
        ip = %ip,
        port,
        "file transfer channel endpoint"
    );
    crate::filetransfer::TransferChannel::connect(&ip, port, row.get("ftkey").unwrap_or("")).await
}

/// Lowercase hex MD5 — the `client_flag_avatar` value the server expects
/// after an avatar upload.
fn md5_hex(data: &[u8]) -> String {
    use md5::Digest;
    let digest = md5::Md5::digest(data);
    let mut out = String::with_capacity(32);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{build_whisper_packet, transfer_ip, WhisperTarget};
    use univox_core::id::MemberId;

    fn peer() -> std::net::SocketAddr {
        "127.0.0.1:9987".parse().unwrap()
    }

    #[test]
    fn transfer_ip_uses_reported_address() {
        assert_eq!(transfer_ip(Some("203.0.113.7"), peer()), "203.0.113.7");
        assert_eq!(transfer_ip(Some("2001:db8::1"), peer()), "2001:db8::1");
    }

    #[test]
    fn transfer_ip_falls_back_to_voice_peer_on_unspecified() {
        // Servers with `filetransfer_ip=0.0.0.0` (the ts3server default)
        // report these shapes; connecting to them directly fails on mobile
        // with ECONNREFUSED (loopback).
        assert_eq!(transfer_ip(Some("0.0.0.0"), peer()), "127.0.0.1");
        assert_eq!(transfer_ip(Some("::"), peer()), "127.0.0.1");
        assert_eq!(transfer_ip(Some(""), peer()), "127.0.0.1");
        assert_eq!(transfer_ip(None, peer()), "127.0.0.1");
    }

    #[test]
    fn transfer_ip_falls_back_on_ipv6_voice_peer() {
        let peer: std::net::SocketAddr = "[2001:db8::5]:9987".parse().unwrap();
        assert_eq!(transfer_ip(Some("0.0.0.0"), peer), "2001:db8::5");
        assert_eq!(transfer_ip(None, peer), "2001:db8::5");
    }

    #[test]
    fn whisper_packet_mixed_targets_layout() {
        // Legacy format: [codec][N][M][cid:8 ×N][clid:16 ×M][opus].
        let targets = vec![
            WhisperTarget::Member(MemberId::from_u64(7)),
            WhisperTarget::Channel(univox_core::id::ChannelId::from_u64(1)),
            WhisperTarget::Channel(univox_core::id::ChannelId::from_u64(2)),
        ];
        let pkt = build_whisper_packet(&targets, &[0xAA, 0xBB]).unwrap();
        assert_eq!(pkt[0], univox_ts3_proto::CODEC_OPUS_VOICE);
        assert_eq!(pkt[1], 2, "channel count");
        assert_eq!(pkt[2], 1, "client count");
        assert_eq!(&pkt[3..11], &1u64.to_be_bytes());
        assert_eq!(&pkt[11..19], &2u64.to_be_bytes());
        assert_eq!(&pkt[19..21], &7u16.to_be_bytes());
        assert_eq!(&pkt[21..], &[0xAA, 0xBB]);
    }

    #[test]
    fn whisper_packet_all_members() {
        let targets: Vec<WhisperTarget> =
            (1..=3).map(|i| WhisperTarget::Member(MemberId::from_u64(i))).collect();
        let pkt = build_whisper_packet(&targets, &[0x01]).unwrap();
        assert_eq!(&pkt[1..3], &[0, 3]);
        assert_eq!(&pkt[3..5], &1u16.to_be_bytes());
        assert_eq!(&pkt[5..7], &2u16.to_be_bytes());
        assert_eq!(&pkt[7..9], &3u16.to_be_bytes());
        assert_eq!(&pkt[9..], &[0x01]);
    }
}
