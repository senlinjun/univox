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
    /// `type`: 0 = template, 1 = regular, 2 = ServerQuery.
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

/// An active temporary password (`servertemppasswordlist`, §8.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TempPassword {
    /// The temp password itself (`pw_clear`; may be empty when the
    /// caller lacks `b_virtualserver_servertemppassword_list` detail).
    pub password: String,
    pub description: String,
    /// Unix timestamps: when the password was created / expires.
    pub start: Option<u64>,
    pub end: Option<u64>,
    /// The channel the password unlocks.
    pub channel: univox_core::id::ChannelId,
}

/// A channel-group assignment (`channelgroupclientlist`, §9.3): `member_db`
/// has channel group `group` inside `channel`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelGroupAssignment {
    pub member_db: DbId,
    pub group: u64,
    pub channel: univox_core::id::ChannelId,
}

/// Per-member connection quality (`clientconnectioninfo`, §11).
/// `downstream` is server→client, `upstream` client→server; losses are
/// fractions (0.0–1.0).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberConnectionInfo {
    pub ping: Option<f32>,
    pub ping_deviation: Option<f32>,
    pub idle_time: Option<Duration>,
    pub downstream_packetloss_speech: Option<f32>,
    pub downstream_packetloss_total: Option<f32>,
    pub upstream_packetloss_speech: Option<f32>,
    pub upstream_packetloss_total: Option<f32>,
    pub downstream_bandwidth_last_second: Option<u64>,
    pub upstream_bandwidth_last_second: Option<u64>,
}

/// Overall connection quality (`serverconnectioninfo`, §11).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerConnectionInfo {
    pub packets_sent_total: Option<u64>,
    pub packets_received_total: Option<u64>,
    pub bytes_sent_total: Option<u64>,
    pub bytes_received_total: Option<u64>,
    pub ping: Option<f32>,
    pub ping_deviation: Option<f32>,
    /// How long this connection has been up.
    pub connected_time: Option<Duration>,
    pub downstream_packetloss_total: Option<f32>,
    pub upstream_packetloss_total: Option<f32>,
}

/// Group list kind column: real servers (3.13.8) send it as `type`
/// (0 = template, 1 = regular, 2 = ServerQuery); the documented
/// `sgtype`/`cgtype` names never appear on the wire.
fn group_kind(row: &Row) -> u8 {
    row.get("type")
        .or_else(|| row.get("sgtype"))
        .or_else(|| row.get("cgtype"))
        .and_then(|v| v.parse().ok())
        .unwrap_or(0)
}

/// Parse an optional f32 row value.
fn row_f32(row: &Row, key: &str) -> Option<f32> {
    row.get(key).and_then(|v| v.parse().ok())
}

/// Parse an optional u64 row value.
fn row_u64(row: &Row, key: &str) -> Option<u64> {
    row.get(key).and_then(|v| v.parse().ok())
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

    // ---- temporary passwords (§8.2) ----

    /// Add a temporary password (§8.2 `servertemppasswordadd`): grants
    /// access to `channel` (overriding its regular password) for `duration`
    /// seconds (None/0 = until server stop). `channel_password` is the
    /// target channel's own password, plaintext — hashed internally like
    /// every channel password; only needed when the channel has one.
    async fn add_temp_password(
        &self,
        channel: &univox_core::id::ChannelId,
        password: &str,
        description: &str,
        duration: Option<Duration>,
        channel_password: Option<&str>,
    ) -> Result<()>;
    /// `servertemppasswordlist` — the active temporary passwords.
    async fn temp_passwords(&self) -> Result<Vec<TempPassword>>;
    /// `servertemppassworddel` — remove by the temp password itself.
    async fn remove_temp_password(&self, password: &str) -> Result<()>;

    // ---- channel group assignment (§9.3) ----

    /// `setclientchannelgroup` — assign `group` (a channel group id from
    /// [`Ts3Ext::channel_groups`]) to `member` inside `channel`. The
    /// member's database id is resolved internally.
    async fn set_member_channel_group(
        &self,
        member: &MemberId,
        channel: &univox_core::id::ChannelId,
        group: u64,
    ) -> Result<()>;
    /// `channelgroupclientlist` — the channel-group assignments of
    /// `channel`, or of the whole server when `channel` is None.
    async fn channel_group_members(
        &self,
        channel: Option<&univox_core::id::ChannelId>,
    ) -> Result<Vec<ChannelGroupAssignment>>;

    // ---- local password verification (§11) ----

    /// Verify a channel password against the session's local hash cache
    /// (same model as the original client, which verifies saved passwords
    /// locally). The cache is filled by every successful
    /// [`Session::join_voice`](univox_core::session::Session::join_voice) /
    /// [`Session::create_channel`](univox_core::session::Session::create_channel)
    /// with a password — the server's own stored hash is salted with the
    /// channel identity and therefore not client-computable.
    /// Channels without a password flag always verify `true`.
    async fn verify_channel_password(
        &self,
        channel: &univox_core::id::ChannelId,
        plaintext: &str,
    ) -> Result<bool>;

    // ---- talk power (§9.5) ----

    /// Request talk power in the current channel. The server relays the
    /// request to privileged listeners as
    /// [`Event::TalkPowerRequested`](univox_core::event::Event::TalkPowerRequested).
    ///
    /// Wire note (3.13.8): the documented `clientupdate
    /// client_talk_request=1` is rejected (1538) for non-zero values and
    /// `client_talk_request_msg` is not accepted at all — this method sends
    /// the accepted `client_talk_request_time` spelling instead, so
    /// `message` is currently not transmitted when the server only speaks
    /// the new spelling.
    async fn request_talk_power(&self, message: Option<&str>) -> Result<()>;
    /// Withdraw the talk power request.
    async fn cancel_talk_power_request(&self) -> Result<()>;
    /// Grant `member` the talk power of `group` (a server group with
    /// `b_client_is_talker`) for `duration` — the group membership is
    /// removed automatically afterwards (locally scheduled; survives only
    /// as long as this session).
    async fn grant_talk_power(
        &self,
        member: &MemberId,
        group: u64,
        duration: Option<Duration>,
    ) -> Result<()>;

    // ---- connection / local info queries (§11) ----

    /// `clientinfo`'s idle time — how long `member` has not sent anything.
    async fn member_idle_time(&self, member: &MemberId) -> Result<Duration>;
    /// `clientconnectioninfo` — per-member connection quality.
    async fn member_connection_info(&self, member: &MemberId)
        -> Result<MemberConnectionInfo>;
    /// `serverconnectioninfo` — this client's overall connection quality.
    async fn server_connection_info(&self) -> Result<ServerConnectionInfo>;

    // ---- icons & banner (§8.1/§11) ----

    /// Upload an icon (§11): stores `data` in the server-wide file area as
    /// `/icon_<crc64>` (the TS3 convention — the id is the CRC64-ECMA of
    /// the content) and returns that id. Apply it with
    /// [`Ts3Ext::set_member_icon`] / [`Ts3Ext::set_channel_icon`].
    async fn upload_icon(&self, data: &[u8]) -> Result<i64>;
    /// Download the icon with `id` (from `/icon_<id>` in the server-wide
    /// file area). Ids come from `client_icon_id` / `channel_icon_id` /
    /// `virtualserver_icon_id` fields.
    async fn download_icon(&self, id: i64) -> Result<Vec<u8>>;
    /// Upload `data` and set it as this client's icon in one step.
    async fn set_member_icon(&self, data: &[u8]) -> Result<i64>;
    /// Set (or clear with 0) a channel's icon.
    async fn set_channel_icon(
        &self,
        channel: &univox_core::id::ChannelId,
        icon_id: i64,
    ) -> Result<()>;
    /// Set the server banner (`serveredit hostbanner_*`). `gfx_url` is the
    /// banner image URL (empty string clears the image); `gfx_interval`
    /// is the rotation interval in seconds; `mode`: 0 = stretch (ignore
    /// aspect), 1 = keep aspect, 2 = only show when the client is on a
    /// banner-capable platform.
    async fn set_host_banner(
        &self,
        url: &str,
        gfx_url: Option<&str>,
        gfx_interval: Option<u64>,
        mode: Option<u8>,
    ) -> Result<()>;
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
                kind: group_kind(&r),
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
                // Real servers send the column as `type` (3.13.8); the
                // documented `cgtype` never appears.
                kind: group_kind(&r),
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

    async fn add_temp_password(
        &self,
        channel: &univox_core::id::ChannelId,
        password: &str,
        description: &str,
        duration: Option<Duration>,
        channel_password: Option<&str>,
    ) -> Result<()> {
        // pw is the temp password itself (returned as pw_clear in the
        // list); tcpw is the target channel's password — hashed like every
        // channel password we send.
        let mut cmd = Command::new("servertemppasswordadd")
            .param("pw", password)
            .param("desc", description)
            .param("tcid", channel.as_u64().unwrap_or(0));
        if let Some(d) = duration {
            cmd = cmd.param("duration", d.as_secs());
        }
        if let Some(cp) = channel_password {
            cmd = cmd.param("tcpw", univox_ts3_proto::hash_password(cp));
        }
        self.exec_ok(cmd).await
    }

    async fn temp_passwords(&self) -> Result<Vec<TempPassword>> {
        // 1281 "database empty result set": no temp passwords active.
        let rows = match self
            .exec_list(
                Command::new("servertemppasswordlist"),
                "notifyservertemppasswordlist",
                "notifyservertemppasswordlistfinished",
            )
            .await
        {
            Ok(rows) => rows,
            Err(Error::Platform { code: 1281, .. }) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        // The server answers inline AND repeats the rows as a
        // notification — dedupe by content.
        let mut out: Vec<TempPassword> = Vec::new();
        for r in rows {
            let pw = TempPassword {
                password: r.get("pw_clear").unwrap_or_default().to_string(),
                description: r.get("desc").unwrap_or_default().to_string(),
                start: r.get("start").and_then(|v| v.parse().ok()),
                end: r.get("end").and_then(|v| v.parse().ok()),
                channel: univox_core::id::ChannelId::from_u64(
                    r.get("tcid").and_then(|v| v.parse().ok()).unwrap_or(0),
                ),
            };
            if !out.contains(&pw) {
                out.push(pw);
            }
        }
        Ok(out)
    }

    async fn remove_temp_password(&self, password: &str) -> Result<()> {
        self.exec_ok(Command::new("servertemppassworddel").param("pw", password))
            .await
    }

    async fn set_member_channel_group(
        &self,
        member: &MemberId,
        channel: &univox_core::id::ChannelId,
        group: u64,
    ) -> Result<()> {
        let uid = self.uid_from_clid(member).await?;
        let db = self
            .dbid_from_uid(&uid)
            .await?
            .ok_or_else(|| Error::Other(format!("no database id for uid {uid}")))?;
        self.exec_ok(
            Command::new("setclientchannelgroup")
                .param("cgid", group)
                .param("cid", channel.as_u64().unwrap_or(0))
                .param("cldbid", db.as_u64().unwrap_or(0)),
        )
        .await
    }

    async fn channel_group_members(
        &self,
        channel: Option<&univox_core::id::ChannelId>,
    ) -> Result<Vec<ChannelGroupAssignment>> {
        let mut cmd = Command::new("channelgroupclientlist");
        if let Some(c) = channel {
            cmd = cmd.param("cid", c.as_u64().unwrap_or(0));
        }
        // 1281 "database empty result set": no assignments — empty list.
        let rows = match self.exec(cmd).await {
            Ok(rows) => rows,
            Err(Error::Platform { code: 1281, .. }) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        Ok(rows
            .into_iter()
            .map(|r| ChannelGroupAssignment {
                member_db: DbId::from_u64(r.get("cldbid").and_then(|v| v.parse().ok()).unwrap_or(0)),
                group: r.get("cgid").and_then(|v| v.parse().ok()).unwrap_or(0),
                channel: univox_core::id::ChannelId::from_u64(
                    r.get("cid").and_then(|v| v.parse().ok()).unwrap_or(0),
                ),
            })
            .collect())
    }

    async fn verify_channel_password(
        &self,
        channel: &univox_core::id::ChannelId,
        plaintext: &str,
    ) -> Result<bool> {
        // No password flag in the book ⇒ trivially verified.
        let flag = self.book().with(|b| {
            b.channels
                .get(channel)
                .and_then(|c| c.extra.get("channel_flag_password").cloned())
        });
        if flag.flatten().as_deref() == Some("0") {
            return Ok(true);
        }
        let cached = self
            .pw_cache
            .lock()
            .unwrap()
            .get(&channel.as_u64().unwrap_or(0))
            .cloned();
        let cached = cached
            .ok_or_else(|| Error::Other(
                "no cached password hash for this channel; join or create it once first".into(),
            ))?;
        Ok(cached == univox_ts3_proto::hash_password(plaintext))
    }

    async fn request_talk_power(&self, message: Option<&str>) -> Result<()> {
        // The documented key (`clientupdate client_talk_request=1`) is
        // rejected by server 3.13.8 with 1538 for any non-zero value, and
        // both `client_talk_request_msg` and `clientconnectioninfo`-style
        // spellings are unknown; the accepted spelling is
        // `client_talk_request_time=<unix ts>` (the relayed
        // `client_talk_request` is a timestamp per the reference book).
        // `message` is kept in the API for servers that still accept the
        // documented message field but is not transmitted on 3.13.8.
        let _ = message;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let cmd = Command::new("clientupdate").param("client_talk_request_time", now.max(1));
        self.exec_ok(cmd).await
    }

    async fn cancel_talk_power_request(&self) -> Result<()> {
        self.exec_ok(Command::new("clientupdate").param("client_talk_request", 0u8))
            .await
    }

    async fn grant_talk_power(
        &self,
        member: &MemberId,
        group: u64,
        duration: Option<Duration>,
    ) -> Result<()> {
        let uid = self.uid_from_clid(member).await?;
        let db = self
            .dbid_from_uid(&uid)
            .await?
            .ok_or_else(|| Error::Other(format!("no database id for uid {uid}")))?;
        self.exec_ok(
            Command::new("servergroupaddclient")
                .param("sgid", group)
                .param("cldbid", db.as_u64().unwrap_or(0)),
        )
        .await?;
        if let Some(d) = duration {
            // Revoke through the connection directly — the task outlives
            // this call and only needs the connection handle.
            let conn = self.conn();
            let dbid = db.as_u64().unwrap_or(0);
            tokio::spawn(async move {
                tokio::time::sleep(d).await;
                if let Err(e) = conn
                    .exec(
                        Command::new("servergroupdelclient")
                            .param("sgid", group)
                            .param("cldbid", dbid),
                    )
                    .await
                {
                    tracing::warn!(error = %e, "talk power auto-revoke failed");
                }
            });
        }
        Ok(())
    }

    async fn member_idle_time(&self, member: &MemberId) -> Result<Duration> {
        // `clientinfo` returns no inline rows on the client protocol;
        // `clientlist -times` carries client_idle_time for everyone.
        let want = member.as_u64().unwrap_or(0).to_string();
        let rows = self.exec(Command::new("clientlist").opt("times")).await?;
        rows.iter()
            .find(|r| r.get("clid") == Some(want.as_str()))
            .and_then(|r| row_u64(r, "client_idle_time"))
            .map(Duration::from_millis)
            .ok_or_else(|| Error::Other("clientlist -times: member not found".into()))
    }

    async fn member_connection_info(
        &self,
        member: &MemberId,
    ) -> Result<MemberConnectionInfo> {
        // Clients push their own stats with `setconnectioninfo` when the
        // server asks; `getconnectioninfo` fetches a member's last pushed
        // row. (`clientconnectioninfo` is ServerQuery-only.)
        let rows = self
            .exec(Command::new("getconnectioninfo").param("clid", member.as_u64().unwrap_or(0)))
            .await?;
        let r = rows.first().ok_or_else(|| Error::Other("getconnectioninfo: no row".into()))?;
        Ok(MemberConnectionInfo {
            ping: row_f32(r, "connection_ping"),
            ping_deviation: row_f32(r, "connection_ping_deviation"),
            idle_time: row_u64(r, "connection_idle_time").map(Duration::from_millis),
            downstream_packetloss_speech: row_f32(r, "connection_server2client_packetloss_speech"),
            downstream_packetloss_total: row_f32(r, "connection_server2client_packetloss_total"),
            upstream_packetloss_speech: row_f32(r, "connection_client2server_packetloss_speech"),
            upstream_packetloss_total: row_f32(r, "connection_client2server_packetloss_total"),
            downstream_bandwidth_last_second: row_u64(
                r,
                "connection_bandwidth_received_last_second_total",
            ),
            upstream_bandwidth_last_second: row_u64(r, "connection_bandwidth_sent_last_second_total"),
        })
    }

    async fn server_connection_info(&self) -> Result<ServerConnectionInfo> {
        // There is no server-aggregate command on the client protocol;
        // report this client's own pushed stats (its connection quality).
        let me = MemberId::from_u64(u64::from(self.clid()));
        let rows = self
            .exec(Command::new("getconnectioninfo").param("clid", me.as_u64().unwrap_or(0)))
            .await?;
        let r = rows.first().ok_or_else(|| Error::Other("getconnectioninfo: no row".into()))?;
        Ok(ServerConnectionInfo {
            packets_sent_total: row_u64(r, "connection_packets_sent_total"),
            packets_received_total: row_u64(r, "connection_packets_received_total"),
            bytes_sent_total: row_u64(r, "connection_bytes_sent_total"),
            bytes_received_total: row_u64(r, "connection_bytes_received_total"),
            ping: row_f32(r, "connection_ping"),
            ping_deviation: row_f32(r, "connection_ping_deviation"),
            connected_time: row_u64(r, "connection_connected_time").map(Duration::from_millis),
            downstream_packetloss_total: row_f32(r, "connection_server2client_packetloss_total"),
            upstream_packetloss_total: row_f32(r, "connection_client2server_packetloss_total"),
        })
    }

    async fn upload_icon(&self, data: &[u8]) -> Result<i64> {
        let id = icon_id(data);
        let name = format!("/icon_{id}");
        // Server-wide icons live in the channel-0 file area; no overwrite
        // flag needed — the name is content-addressed.
        let mut up = self
            .upload_file_stream(
                &univox_core::id::ChannelId::from_u64(0),
                &name,
                data.len() as u64,
                None,
            )
            .await?;
        up.write_chunk(data).await?;
        up.finish().await?;
        Ok(id)
    }

    async fn download_icon(&self, id: i64) -> Result<Vec<u8>> {
        self.download_file(&univox_core::id::ChannelId::from_u64(0), &format!("/icon_{id}"))
            .await
    }

    async fn set_member_icon(&self, data: &[u8]) -> Result<i64> {
        let id = self.upload_icon(data).await?;
        // Client icons live on the database id as `i_icon_id` (same
        // permission storage as channel icons).
        let db = self.own_dbid().await?;
        self.exec_ok(
            Command::new("clientaddperm")
                .param("cldbid", db.as_u64().unwrap_or(0))
                .param("permsid", "i_icon_id")
                .param("permvalue", id)
                .param("permskip", 0u8)
                .param("permnegated", 0u8),
        )
        .await?;
        Ok(id)
    }

    async fn set_channel_icon(
        &self,
        channel: &univox_core::id::ChannelId,
        icon_id: i64,
    ) -> Result<()> {
        // Icons are stored as the `i_icon_id` permission — `channeledit
        // channel_icon_id=…` is rejected with 1538 on 3.13.8.
        let cid = channel.as_u64().unwrap_or(0);
        if icon_id == 0 {
            self.exec_ok(Command::new("channeldelperm").param("cid", cid).param("permsid", "i_icon_id"))
                .await
        } else {
            self.exec_ok(
                Command::new("channeladdperm")
                    .param("cid", cid)
                    .param("permsid", "i_icon_id")
                    // permvalue parses as i32; servers echo the unsigned view.
                    .param("permvalue", icon_id as i32)
                    .param("permskip", 0u8)
                    .param("permnegated", 0u8),
            )
            .await
        }
    }

    async fn set_host_banner(
        &self,
        url: &str,
        gfx_url: Option<&str>,
        gfx_interval: Option<u64>,
        mode: Option<u8>,
    ) -> Result<()> {
        let mut cmd = Command::new("serveredit").param("virtualserver_hostbanner_url", url);
        if let Some(g) = gfx_url {
            cmd = cmd.param("virtualserver_hostbanner_gfx_url", g);
        }
        if let Some(i) = gfx_interval {
            cmd = cmd.param("virtualserver_hostbanner_gfx_interval", i);
        }
        if let Some(m) = mode {
            cmd = cmd.param("virtualserver_hostbanner_mode", m);
        }
        self.exec_ok(cmd).await
    }
}

/// The icon id for icon `data`: the low 32 bits of the CRC64-ECMA of the
/// content (unsigned view) — the TS3 convention for `/icon_<id>` names.
/// Truncation is required because icons ride on the permission value
/// (`i_icon_id`), which is a 32-bit integer; servers echo the unsigned
/// form in `channel_icon_id`/`client_icon_id`.
pub fn icon_id(data: &[u8]) -> i64 {
    crc64_ecma(data) as u32 as i64
}

/// CRC64-ECMA (poly 0x42F0E1EBA9EA3693, init 0, no reflection).
fn crc64_ecma(data: &[u8]) -> u64 {
    const POLY: u64 = 0x42F0_E1EB_A9EA_3693;
    let mut crc: u64 = 0;
    for &b in data {
        crc ^= u64::from(b) << 56;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000_0000_0000 != 0 {
                (crc << 1) ^ POLY
            } else {
                crc << 1
            };
        }
    }
    crc
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
    fn crc64_ecma_check_vector() {
        // CRC-64/ECMA-681 catalogue check value for "123456789".
        assert_eq!(super::crc64_ecma(b"123456789"), 0x6c40df5f0b497347);
        // icon_id is the unsigned 32-bit truncation of the same value.
        assert_eq!(super::icon_id(b"123456789"), 0x0b497347u32 as i64);
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
