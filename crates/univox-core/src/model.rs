//! Unified data model (FEATURES.md §1). Platform-specific fields live in
//! driver extension types (e.g. [`ChannelExtValues`] carried as extra
//! attributes until the driver's `Ext` types are engaged).

use std::collections::BTreeMap;
use std::time::Duration;

use crate::id::{ChannelId, DbId, MemberId, MessageId, RoleId, ServerId};

/// What a channel carries. TS3 channels are both (kind recorded twice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelKind {
    #[default]
    Voice,
    Text,
}

/// Channel persistence (TS3 permanence mapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Permanence {
    Permanent,
    SemiPermanent,
    #[default]
    Temporary,
}

/// A server (TS3 virtual server / KOOK guild / OOPZ area).
#[derive(Debug, Clone, Default)]
pub struct Server {
    pub id: ServerId,
    pub name: String,
    pub icon: Option<String>,
    pub description: Option<String>,
    pub host_message: Option<String>,
    pub host_message_mode: Option<HostMessageMode>,
    pub member_count: u64,
    pub member_limit: u64,
    pub created_at: Option<crate::time::Timestamp>,
    pub version: Option<String>,
    pub platform: Option<String>,
    pub region: Option<String>,
    pub extra: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostMessageMode {
    None,
    Log,
    Modal,
    ModalQuit,
}

/// A channel. TS3 channels are voice+text simultaneously (`kind` records
/// both); KOOK/OOPZ have separate voice/text channels.
#[derive(Debug, Clone, Default)]
pub struct Channel {
    pub id: ChannelId,
    pub server_id: ServerId,
    pub kinds: u32, // bit 0 = voice, bit 1 = text; see ChannelKind::bit()
    pub name: String,
    pub topic: Option<String>,
    pub description: Option<String>,
    pub parent_id: Option<ChannelId>,
    /// Sort key among siblings (TS3 `channel_order` predecessor).
    pub order: u64,
    pub position: i64,
    pub password_protected: bool,
    pub user_limit: u64,
    pub is_default: bool,
    pub permanence: Permanence,
    pub extra: BTreeMap<String, String>,
}

impl ChannelKind {
    pub fn bit(self) -> u32 {
        match self {
            ChannelKind::Voice => 1,
            ChannelKind::Text => 2,
        }
    }
}

impl Channel {
    pub fn has_kind(&self, kind: ChannelKind) -> bool {
        self.kinds & kind.bit() != 0
    }

    pub fn set_kind(&mut self, kind: ChannelKind, on: bool) {
        if on {
            self.kinds |= kind.bit();
        } else {
            self.kinds &= !kind.bit();
        }
    }
}

/// Online presence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnlineState {
    #[default]
    Online,
    Idle,
    Offline,
    Invisible,
}

/// A member (TS3 client / KOOK user / OOPZ user).
#[derive(Debug, Clone, Default)]
pub struct Member {
    pub id: MemberId,
    pub server_id: ServerId,
    pub nickname: String,
    pub avatar: Option<String>,
    pub is_bot: bool,
    pub online: OnlineState,
    pub role_ids: Vec<RoleId>,
    pub channel_id: Option<ChannelId>,
    pub joined_at: Option<crate::time::Timestamp>,
    pub extra: BTreeMap<String, String>,
}

/// Runtime voice/member state, property-level updated (FEATURES.md §1).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemberState {
    pub input_muted: bool,
    pub output_muted: bool,
    pub deafened: bool,
    pub away: bool,
    pub away_message: Option<String>,
    pub speaking: bool,
    pub talk_power: i64,
    pub channel_commander: bool,
    pub priority_speaker: bool,
    pub recording: bool,
}

/// A role/server-group.
#[derive(Debug, Clone, Default)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub color: Option<u32>,
    pub position: i64,
    pub permissions: Vec<String>,
    pub extra: BTreeMap<String, String>,
}

/// Message routing targets (FEATURES.md §7.1).
#[derive(Debug, Clone, PartialEq)]
pub enum MessageTarget {
    Channel(ChannelId),
    Direct(MemberId),
    Server,
    /// TS3 poke.
    Poke(MemberId),
    /// TS3 global message (`gm`) — query level only.
    Global,
}

/// A message (text/poke/offline).
#[derive(Debug, Clone, Default)]
pub struct Message {
    pub id: MessageId,
    pub target: Option<MessageTarget>,
    pub author: Option<MemberId>,
    pub author_name: String,
    pub content: String,
    pub created_at: Option<crate::time::Timestamp>,
    pub extra: BTreeMap<String, String>,
}

/// Voice state of a member (FEATURES.md §1 VoiceState).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct VoiceState {
    pub member_id: MemberId,
    pub channel_id: Option<ChannelId>,
    pub self_mute: bool,
    pub self_deaf: bool,
    pub speaking: bool,
}

/// The session's own view inside the server.
#[derive(Debug, Clone, Default)]
pub struct SelfMember {
    pub member_id: Option<MemberId>,
    pub db_id: Option<DbId>,
    pub server_id: Option<ServerId>,
    pub channel_id: Option<ChannelId>,
    pub state: MemberState,
    pub nickname: String,
}

/// Connection quality statistics (FEATURES.md §2.5).
#[derive(Debug, Clone, Default)]
pub struct ConnectionStats {
    pub ping: Option<Duration>,
    pub packet_loss: f32,
    pub bandwidth_up: u64,
    pub bandwidth_down: u64,
    pub reconnect_count: u64,
}

/// Channel creation/edit options (FEATURES.md §8.1).
#[derive(Debug, Clone, Default)]
pub struct ChannelOptions {
    pub name: String,
    pub kind: ChannelKind,
    pub parent: Option<ChannelId>,
    pub topic: Option<String>,
    pub description: Option<String>,
    pub password: Option<String>,
    pub user_limit: Option<u64>,
    pub default_channel: bool,
    pub permanence: Permanence,
    pub delete_delay: Option<Duration>,
    pub extra: BTreeMap<String, String>,
}

/// Disconnect reasons (FEATURES.md §2.2).
#[derive(Debug, Clone, PartialEq)]
pub enum DisconnectReason {
    Requested { message: Option<String> },
    Timeout,
    ServerStop,
    Kicked { by: Option<String>, message: Option<String> },
    Banned { by: Option<String>, message: Option<String> },
    ServerDeleted,
    Network(String),
    Auth(String),
    Other(String),
}

impl DisconnectReason {
    /// Whether an automatic reconnect is appropriate (FEATURES.md §2.2:
    /// kicks and bans never reconnect).
    pub fn should_reconnect(&self) -> bool {
        matches!(
            self,
            DisconnectReason::Timeout | DisconnectReason::ServerStop | DisconnectReason::Network(_)
        )
    }
}

/// Why a member left the view (FEATURES.md §4). Structured so consumers can
/// map quit/move/kick/ban kinds directly; platform reason ids that don't map
/// cleanly are preserved verbatim in [`MemberLeftReason::Other`].
#[derive(Debug, Clone, PartialEq)]
pub enum MemberLeftReason {
    /// Left the visible view normally (e.g. switched to another subscribed
    /// channel — pair with the following `MemberJoined` for a move).
    Left,
    /// Moved out of the view by an invoker (TS3 reasonid 1).
    Moved { by: Option<MemberId> },
    /// View lost by unsubscribing (TS3 reasonid 2).
    Unsubscribed,
    /// Connection timed out (TS3 reasonid 3).
    Timeout,
    /// Kicked from the previous channel (TS3 reasonid 4).
    ChannelKicked { by: Option<MemberId>, message: String },
    /// Kicked from the server (TS3 reasonid 5).
    ServerKicked { by: Option<MemberId>, message: String },
    /// Banned (TS3 reasonid 6).
    Banned { by: Option<MemberId>, message: String },
    /// The server stopped or shut down (TS3 reasonid 7/11).
    ServerStop,
    /// Disconnected on their own (TS3 reasonid 8).
    Quit,
    /// Anything else — carries the raw platform encoding (e.g.
    /// `reasonid=9`).
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChannelId;

    #[test]
    fn channel_kinds() {
        let mut ch = Channel::default();
        ch.set_kind(ChannelKind::Voice, true);
        ch.set_kind(ChannelKind::Text, true);
        assert!(ch.has_kind(ChannelKind::Voice));
        assert!(ch.has_kind(ChannelKind::Text));
        ch.set_kind(ChannelKind::Text, false);
        assert!(!ch.has_kind(ChannelKind::Text));

        let _id: ChannelId = 1u64.into();
    }

    #[test]
    fn disconnect_reconnect_policy() {
        assert!(DisconnectReason::Timeout.should_reconnect());
        assert!(DisconnectReason::ServerStop.should_reconnect());
        assert!(!DisconnectReason::Kicked {
            by: None,
            message: None
        }
        .should_reconnect());
        assert!(!DisconnectReason::Banned {
            by: None,
            message: None
        }
        .should_reconnect());
    }
}
