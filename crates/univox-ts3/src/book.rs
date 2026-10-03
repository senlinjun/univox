//! TS3 command/notification → unified [`Book`] updates + [`Event`]s
//! (FEATURES.md §4/§5.3 mapping table).

use univox_core::event::{Event, RawEvent};
use univox_core::id::{ChannelId, MemberId, MessageId};
use univox_core::model::{
    Channel, ChannelKind, HostMessageMode, Member, MemberState, Message, MessageTarget,
    OnlineState, Permanence, Server, VoiceState,
};
use univox_core::platform::Platform;
use univox_ts3_proto::{Command, RowExt};

use univox_ts3_proto::Error;

/// Which connection role a command stream belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamOrigin {
    /// The native client protocol connection.
    Client,
    /// A ServerQuery management connection.
    Query,
}

fn member_id(raw: &str) -> MemberId {
    MemberId::from_u64(raw.parse().unwrap_or(0))
}

fn channel_id(raw: &str) -> ChannelId {
    ChannelId::from_u64(raw.parse().unwrap_or(0))
}

fn ts3_raw(name: &str, cmd: &Command) -> Event {
    Event::Raw(RawEvent {
        platform: Platform::Ts3,
        name: name.to_string(),
        payload: cmd.params.first().cloned().unwrap_or_default(),
    })
}

fn parse_channel(row: &[(String, String)]) -> Channel {
    let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
    let mut ch = Channel {
        id: channel_id(get("cid").unwrap_or("0")),
        parent_id: get("pid").and_then(|p| p.parse::<u64>().ok()).map(ChannelId::from_u64),
        name: get("channel_name").unwrap_or("").to_string(),
        topic: get("channel_topic").filter(|s| !s.is_empty()).map(String::from),
        description: get("channel_description").filter(|s| !s.is_empty()).map(String::from),
        order: get("channel_order").and_then(|v| v.parse().ok()).unwrap_or(0),
        password_protected: get("channel_flag_password").is_some_and(|v| v == "1"),
        user_limit: get("channel_maxclients")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0),
        is_default: get("channel_flag_default").is_some_and(|v| v == "1"),
        permanence: if get("channel_flag_permanent").is_some_and(|v| v == "1") {
            Permanence::Permanent
        } else if get("channel_flag_semi_permanent").is_some_and(|v| v == "1") {
            Permanence::SemiPermanent
        } else {
            Permanence::Temporary
        },
        ..Default::default()
    };
    // TS3 channels carry voice and text simultaneously.
    ch.set_kind(ChannelKind::Voice, true);
    ch.set_kind(ChannelKind::Text, true);
    for (k, v) in row {
        if k != "cid"
            && k != "pid"
            && k != "channel_name"
            && !ch.extra.contains_key(k)
        {
            ch.extra.insert(k.clone(), v.clone());
        }
    }
    ch
}

fn parse_member_state(row: &[(String, String)]) -> MemberState {
    let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
    MemberState {
        input_muted: get("client_input_muted").is_some_and(|v| v == "1"),
        output_muted: get("client_output_muted").is_some_and(|v| v == "1"),
        deafened: get("client_output_muted").is_some_and(|v| v == "1")
            && get("client_input_muted").is_some_and(|v| v == "1"),
        away: get("client_away").is_some_and(|v| v == "1"),
        away_message: get("client_away_message").filter(|s| !s.is_empty()).map(String::from),
        speaking: false,
        talk_power: get("client_talk_power")
            .map(|v| v.parse::<i64>().unwrap_or(0))
            .unwrap_or(0),
        channel_commander: get("client_is_channel_commander").is_some_and(|v| v == "1"),
        priority_speaker: get("client_is_priority_speaker").is_some_and(|v| v == "1"),
        recording: get("client_is_recording").is_some_and(|v| v == "1"),
    }
}

fn parse_member(row: &[(String, String)]) -> Member {
    let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
    Member {
        id: member_id(get("clid").unwrap_or(get("client_id").unwrap_or("0"))),
        nickname: get("client_nickname").unwrap_or("").to_string(),
        online: if get("client_type").is_some_and(|v| v == "1") {
            OnlineState::Online
        } else {
            OnlineState::Online
        },
        // Regular rows use `cid`; cliententerview uses `ctid` as the target.
        channel_id: get("cid")
            .or_else(|| get("ctid"))
            .and_then(|p| p.parse::<u64>().ok())
            .map(ChannelId::from_u64),
        extra: row.iter().cloned().collect(),
        ..Default::default()
    }
}

/// Apply one incoming command to the book. Returns the unified events that
/// this command produced (initial full-sync dumps produce no events).
pub fn apply_to_book(
    book: &univox_core::Book,
    self_clid: u64,
    cmd: &Command,
    origin: StreamOrigin,
) -> Vec<Event> {
    let _ = origin;
    // Pushed list dumps arrive nameless; their first key identifies them.
    let first_key = cmd
        .params
        .first()
        .and_then(|r| r.first())
        .map(|(k, _)| k.as_str())
        .unwrap_or("");
    let kind = match cmd.name.as_str() {
        "" => match first_key {
            // Field name → originating command.
            "cid" => "channellist",
            "clid" => "clientlist",
            "sgid" => "servergrouplist",
            "cgid" => "channelgrouplist",
            "virtualserver_id" | "virtualserver_name" => "initserver",
            "msgid" => "messagelist",
            other => other,
        },
        name => name,
    };
    match kind {
        "initserver" => apply_initserver(book, self_clid, cmd),
        "channellist" => apply_channellist(book, cmd),
        "clientlist" | "notifycliententerview" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let member = parse_member(row);
                let is_new = book.member(&member.id).is_none();
                book.with_mut(|b| {
                    b.members.insert(member.id.clone(), member.clone());
                    let state = parse_member_state(row);
                    let vs = VoiceState {
                        member_id: member.id.clone(),
                        channel_id: member.channel_id.clone(),
                        self_mute: state.input_muted,
                        self_deaf: state.output_muted,
                        speaking: false,
                    };
                    b.member_states.insert(member.id.clone(), state);
                    b.voice_states.insert(member.id.clone(), vs);
                });
                if cmd.name == "notifycliententerview" {
                    if is_new {
                        events.push(Event::MemberJoined { member });
                    } else {
                        events.push(Event::MemberUpdated { member });
                    }
                }
            }
            events
        }
        "notifyplugincmd" => {
            let get = |k: &str| cmd.get(k);
            let member = get("invokerid")
                .or_else(|| get("clid"))
                .map(member_id);
            let target = match get("targetmode").unwrap_or("3") {
                "1" => univox_core::event::PluginCommandTarget::Single,
                "2" => univox_core::event::PluginCommandTarget::CurrentTab,
                "3" => univox_core::event::PluginCommandTarget::Clients,
                _ => univox_core::event::PluginCommandTarget::All,
            };
            vec![Event::PluginCommandReceived {
                member,
                payload: get("data").unwrap_or("").as_bytes().to_vec(),
                target,
            }]
        }
        "notifyclientleftview" | "notifyclientdisconnect" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
                let clid = get("clid").unwrap_or("0");
                let id = member_id(clid);
                let reason = get("reasonid").unwrap_or("").to_string();
                book.with_mut(|b| {
                    b.members.remove(&id);
                    b.member_states.remove(&id);
                    b.voice_states.remove(&id);
                });
                events.push(Event::MemberLeft {
                    id,
                    reason: format!("reasonid={reason}"),
                });
            }
            events
        }
        "notifyclientmoved" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
                let id = member_id(get("clid").unwrap_or("0"));
                let new_channel = channel_id(get("ctid").unwrap_or("0"));
                book.with_mut(|b| {
                    if let Some(m) = b.members.get_mut(&id) {
                        m.channel_id = Some(new_channel.clone());
                    }
                    if let Some(vs) = b.voice_states.get_mut(&id) {
                        vs.channel_id = Some(new_channel.clone());
                    }
                });
                events.push(Event::ClientMoved {
                    member: id,
                    channel: new_channel,
                    invoker: None,
                });
            }
            events
        }
        "notifytextmessage" => {
            let get = |k: &str| cmd.get(k);
            let invoker_id = member_id(get("invokerid").unwrap_or("0"));
            let target = match get("targetmode").unwrap_or("0") {
                "1" => MessageTarget::Direct(member_id(get("target").unwrap_or("0"))),
                "2" => MessageTarget::Channel(ChannelId::from_u64(0)), // own channel
                _ => MessageTarget::Server,
            };
            let message = Message {
                id: MessageId::from_string(format!(
                    "ts3-{}",
                    chrono_like_id()
                )),
                target: Some(target),
                author: Some(invoker_id),
                author_name: get("invokername").unwrap_or("").to_string(),
                content: get("msg").unwrap_or("").to_string(),
                ..Default::default()
            };
            vec![Event::MessageCreated { message }]
        }
        "notifyclientupdated" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let member = parse_member(row);
                book.with_mut(|b| {
                    if let Some(m) = b.members.get_mut(&member.id) {
                        // Update rows only carry the changed fields — keep
                        // the known nickname when the row has none.
                        if !member.nickname.is_empty() {
                            m.nickname = member.nickname.clone();
                        }
                        m.extra.extend(member.extra.clone());
                    }
                    let state = parse_member_state(row);
                    b.member_states.insert(member.id.clone(), state);
                });
                events.push(Event::MemberUpdated { member });
            }
            events
        }
        "notifychanneledited" | "notifychannelcreated" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let channel = parse_channel(row);
                let created = cmd.name == "notifychannelcreated";
                book.with_mut(|b| {
                    b.channels.insert(channel.id.clone(), channel.clone());
                });
                events.push(if created {
                    Event::ChannelCreated { channel }
                } else {
                    Event::ChannelUpdated { channel }
                });
            }
            events
        }
        "notifychanneldeleted" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let id = channel_id(row.get("cid").unwrap_or("0"));
                book.with_mut(|b| {
                    b.channels.remove(&id);
                });
                events.push(Event::ChannelDeleted { id });
            }
            events
        }
        "notifychannelmoved" => {
            let mut events = Vec::new();
            for row in cmd.rows() {
                let get = |k: &str| row.iter().find(|(kk, _)| kk == k).map(|(_, v)| v.as_str());
                let id = channel_id(get("cid").unwrap_or("0"));
                let parent = get("cpid").and_then(|p| p.parse::<u64>().ok()).map(ChannelId::from_u64);
                let order: u64 = get("order").and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
                book.with_mut(|b| {
                    if let Some(c) = b.channels.get_mut(&id) {
                        c.parent_id = parent.clone();
                        c.order = order;
                    }
                });
                events.push(Event::ChannelMoved { id, parent, order });
            }
            events
        }
        "channellistfinished" | "notifyclientsubscription" | "error" => Vec::new(),
        other => {
            if other.starts_with("notify") {
                vec![ts3_raw(other, cmd)]
            } else {
                Vec::new()
            }
        }
    }
}

fn apply_initserver(book: &univox_core::Book, self_clid: u64, cmd: &Command) -> Vec<Event> {
    let get = |k: &str| cmd.get(k);
    let server = Server {
        id: univox_core::id::ServerId::from_u64(
            get("virtualserver_id").map(|v| v.parse::<u64>().ok().unwrap_or(0)).unwrap_or(1),
        ),
        name: get("virtualserver_name").unwrap_or("").to_string(),
        host_message: get("virtualserver_welcomemessage").filter(|s| !s.is_empty()).map(String::from),
        host_message_mode: Some(HostMessageMode::Log),
        member_count: get("virtualserver_clientsonline")
            .map(|v| v.parse::<u64>().ok().unwrap_or(0))
            .unwrap_or(0),
        member_limit: get("virtualserver_maxclients")
            .map(|v| v.parse::<u64>().ok().unwrap_or(0))
            .unwrap_or(0),
        version: get("virtualserver_version").map(String::from),
        ..Default::default()
    };
    let channel_id_of_self = get("client_channel_id")
        .and_then(|v| v.parse::<u64>().ok())
        .map(ChannelId::from_u64);
    book.with_mut(|b| {
        b.server = Some(server);
        b.self_member.member_id = Some(MemberId::from_u64(self_clid));
        b.self_member.channel_id = channel_id_of_self;
        b.self_member.nickname = get("client_nickname").unwrap_or("").to_string();
    });
    Vec::new()
}

fn apply_channellist(book: &univox_core::Book, cmd: &Command) -> Vec<Event> {
    book.with_mut(|b| {
        for row in cmd.rows() {
            let channel = parse_channel(row);
            b.channels.insert(channel.id.clone(), channel);
        }
    });
    Vec::new()
}

fn chrono_like_id() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Build a `sendtextmessage` command from a unified target.
pub fn sendtextmessage_command(
    target: &MessageTarget,
    content: &str,
) -> std::result::Result<Command, Error> {
    let (mode, target_id) = match target {
        MessageTarget::Channel(ch) => (2u8, ch.as_u64().unwrap_or(0)),
        MessageTarget::Direct(m) => (1u8, m.as_u64().unwrap_or(0)),
        MessageTarget::Server => (3u8, 0),
        MessageTarget::Poke(_) | MessageTarget::Global => {
            return Err(Error::Other(
                "poke/global targets need dedicated commands".into(),
            ))
        }
    };
    Ok(Command::new("sendtextmessage")
        .param("targetmode", mode)
        .param("target", target_id)
        .param("msg", content))
}

#[cfg(test)]
mod tests {
    use super::*;
    use univox_core::Book;

    fn book() -> Book {
        Book::default()
    }

    #[test]
    fn channellist_populates_book() {
        let b = book();
        let cmd = Command::parse("cid=1 pid=0 channel_name=Default\\sChannel channel_flag_default=1|cid=2 pid=0 channel_name=Second").unwrap();
        let events = apply_to_book(&b, 1, &cmd, StreamOrigin::Client);
        assert!(events.is_empty(), "initial sync produces no events");
        assert_eq!(b.channels().len(), 2);
        let ch = b.channel(&1u64.into()).unwrap();
        assert_eq!(ch.name, "Default Channel");
        assert!(ch.is_default);
        assert!(ch.has_kind(ChannelKind::Voice) && ch.has_kind(ChannelKind::Text));
    }

    #[test]
    fn enterview_and_leftview() {
        let b = book();
        let enter = Command::parse("notifycliententerview cfid=0 ctid=1 clid=5 client_nickname=Tester client_type=0 client_input_muted=0").unwrap();
        let events = apply_to_book(&b, 1, &enter, StreamOrigin::Client);
        assert!(events.iter().any(|e| matches!(e, Event::MemberJoined { .. })));
        assert!(b.member(&5u64.into()).is_some());
        assert_eq!(b.member(&5u64.into()).unwrap().channel_id, Some(1u64.into()));

        let left = Command::parse("notifyclientleftview cfid=1 ctid=0 clid=5 reasonid=8").unwrap();
        let events = apply_to_book(&b, 1, &left, StreamOrigin::Client);
        assert!(events.iter().any(|e| matches!(e, Event::MemberLeft { .. })));
        assert!(b.member(&5u64.into()).is_none());
    }

    #[test]
    fn text_message_event() {
        let b = book();
        let msg = Command::parse("notifytextmessage targetmode=1 target=2 msg=Hello invokerid=3 invokername=Bob").unwrap();
        let events = apply_to_book(&b, 1, &msg, StreamOrigin::Client);
        match &events[0] {
            Event::MessageCreated { message } => {
                assert_eq!(message.content, "Hello");
                assert_eq!(message.author, Some(3u64.into()));
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn sendtextmessage_targets() {
        let cmd = sendtextmessage_command(&MessageTarget::Channel(7u64.into()), "hi").unwrap();
        assert_eq!(cmd.get("targetmode"), Some("2"));
        assert_eq!(cmd.get("target"), Some("7"));
        let cmd = sendtextmessage_command(&MessageTarget::Server, "hi").unwrap();
        assert_eq!(cmd.get("targetmode"), Some("3"));
        assert!(sendtextmessage_command(&MessageTarget::Poke(1u64.into()), "x").is_err());
    }
}
