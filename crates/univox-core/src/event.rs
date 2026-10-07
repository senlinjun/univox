//! Unified event system (FEATURES.md §5): a per-session bus with dual
//! consumption — `async Stream` subscription and callback registration —
//! with local filtering and `Raw` passthrough for unmapped platform events.

use std::sync::Arc;
use tokio::sync::broadcast;

use crate::id::{ChannelId, MemberId};
use crate::model::{
    Channel, ClientMoveReason, DisconnectReason, Member, MemberLeftReason, Message, Server,
};

/// An unmapped platform event, passed through verbatim (G3: no information
/// loss).
#[derive(Debug, Clone)]
pub struct RawEvent {
    pub platform: crate::platform::Platform,
    /// Platform-native event name (e.g. TS3 notification name).
    pub name: String,
    /// Key/value payload (platform-agnostic encoding of the native data).
    pub payload: Vec<(String, String)>,
}

/// Unified event set (FEATURES.md §5.2).
#[derive(Debug, Clone)]
pub enum Event {
    // Connection
    Connecting,
    Authenticating,
    Connected,
    TemporarilyDisconnected { reason: DisconnectReason },
    Reconnecting,
    Reconnected,
    Closed { reason: DisconnectReason },
    /// TS3: identity security level increased (FEATURES.md §3).
    IdentityLevelIncreased { level: u32 },

    // Server
    ServerUpdated { server: Server },
    HostMessageChanged { message: Option<String> },
    /// Removed from the server (kick/ban/guild deleted).
    SelfRemoved { reason: DisconnectReason },

    // Channels
    ChannelCreated { channel: Channel },
    ChannelUpdated { channel: Channel },
    ChannelDeleted { id: ChannelId },
    ChannelMoved { id: ChannelId, parent: Option<ChannelId>, order: u64 },

    // Members
    MemberJoined { member: Member },
    /// Structured leave reason (kick/ban/move/quit — see
    /// [`MemberLeftReason`]).
    MemberLeft { id: MemberId, reason: MemberLeftReason },
    MemberOnline { id: MemberId },
    MemberOffline { id: MemberId },
    MemberUpdated { member: Member },
    RoleAssigned { member: MemberId, role: crate::id::RoleId },
    RoleRevoked { member: MemberId, role: crate::id::RoleId },

    // Voice
    SelfVoiceJoined { channel: ChannelId },
    SelfVoiceLeft { channel: ChannelId, kicked: bool },
    MemberVoiceJoined { member: MemberId, channel: ChannelId },
    MemberVoiceLeft { member: MemberId, channel: ChannelId },
    /// `whispering` is TS3-only: the audio arrived as a whisper packet
    /// (S2CWhisper) instead of normal channel voice (FEATURES.md §6.4).
    SpeakingStarted { member: MemberId, whispering: bool },
    SpeakingStopped { member: MemberId, whispering: bool },
    /// TS3: someone requested talk power (FEATURES.md §9.5).
    TalkPowerRequested { member: MemberId, message: String },
    AudioCanSendChanged { can: bool },
    AudioCanReceiveChanged { can: bool },

    // Messages
    MessageCreated { message: Message },
    MessageEdited { message: Message },
    MessageDeleted { id: crate::id::MessageId },
    /// KOOK only (Ext).
    ReactionAdded { message: crate::id::MessageId, emoji: String, member: MemberId },
    ReactionRemoved { message: crate::id::MessageId, emoji: String, member: MemberId },
    PinnedMessageChanged { message: crate::id::MessageId, pinned: bool },
    /// KOOK card button click (Ext).
    ButtonClicked { message: crate::id::MessageId, member: MemberId, value: String },

    // Moderation
    MemberKicked { id: MemberId, by: Option<MemberId>, reason: Option<String> },
    MemberBanned { id: MemberId, by: Option<MemberId>, reason: Option<String> },
    TextMuteChanged { member: MemberId, muted: bool },
    VoiceMuteChanged { member: MemberId, muted: bool },
    /// Member moved by an admin.
    /// `reasonid`/`reasonmsg` preserved — a channel kick arrives on this
    /// same notification as `ClientMoveReason::ChannelKicked`.
    ClientMoved {
        member: MemberId,
        channel: ChannelId,
        invoker: Option<MemberId>,
        reason: ClientMoveReason,
    },

    /// TS3: plugin command relay (Ext, FEATURES.md §11.1).
    PluginCommandReceived { member: Option<MemberId>, payload: Vec<u8>, target: PluginCommandTarget },

    /// Unmapped platform events pass through verbatim.
    Raw(RawEvent),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginCommandTarget {
    Single,
    CurrentTab,
    Clients,
    All,
}

/// Local event filters, applied before delivery to a subscriber.
#[derive(Debug, Clone, Default)]
pub struct EventFilter {
    /// Only these categories (empty = all).
    pub categories: Vec<Category>,
    /// Only these channels (voice/channel-related events).
    pub channels: Vec<ChannelId>,
    /// Exclude categories.
    pub exclude: Vec<Category>,
}

/// Event categories used for filtering (mirrors FEATURES.md §5.2 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Connection,
    Server,
    Channel,
    Member,
    Voice,
    Message,
    Moderation,
    Other,
}

impl Event {
    pub fn category(&self) -> Category {
        match self {
            Event::Connecting
            | Event::Authenticating
            | Event::Connected
            | Event::TemporarilyDisconnected { .. }
            | Event::Reconnecting
            | Event::Reconnected
            | Event::Closed { .. }
            | Event::IdentityLevelIncreased { .. } => Category::Connection,
            Event::ServerUpdated { .. }
            | Event::HostMessageChanged { .. }
            | Event::SelfRemoved { .. } => Category::Server,
            Event::ChannelCreated { .. }
            | Event::ChannelUpdated { .. }
            | Event::ChannelDeleted { .. }
            | Event::ChannelMoved { .. } => Category::Channel,
            Event::MemberJoined { .. }
            | Event::MemberLeft { .. }
            | Event::MemberOnline { .. }
            | Event::MemberOffline { .. }
            | Event::MemberUpdated { .. }
            | Event::RoleAssigned { .. }
            | Event::RoleRevoked { .. } => Category::Member,
            Event::SelfVoiceJoined { .. }
            | Event::SelfVoiceLeft { .. }
            | Event::MemberVoiceJoined { .. }
            | Event::MemberVoiceLeft { .. }
            | Event::SpeakingStarted { .. }
            | Event::SpeakingStopped { .. }
            | Event::TalkPowerRequested { .. }
            | Event::AudioCanSendChanged { .. }
            | Event::AudioCanReceiveChanged { .. }
            | Event::PluginCommandReceived { .. } => Category::Voice,
            Event::MessageCreated { .. }
            | Event::MessageEdited { .. }
            | Event::MessageDeleted { .. }
            | Event::ReactionAdded { .. }
            | Event::ReactionRemoved { .. }
            | Event::PinnedMessageChanged { .. }
            | Event::ButtonClicked { .. } => Category::Message,
            Event::MemberKicked { .. }
            | Event::MemberBanned { .. }
            | Event::TextMuteChanged { .. }
            | Event::VoiceMuteChanged { .. }
            | Event::ClientMoved { .. } => Category::Moderation,
            Event::Raw(_) => Category::Other,
        }
    }

    fn matches_channel(&self, channels: &[ChannelId]) -> bool {
        if channels.is_empty() {
            return true;
        }
        let related: Option<ChannelId> = match self {
            Event::ChannelCreated { channel } => Some(channel.id.clone()),
            Event::ChannelUpdated { channel } => Some(channel.id.clone()),
            Event::ChannelDeleted { id } => Some(id.clone()),
            Event::ChannelMoved { id, .. } => Some(id.clone()),
            Event::SelfVoiceJoined { channel } => Some(channel.clone()),
            Event::SelfVoiceLeft { channel, .. } => Some(channel.clone()),
            Event::MemberVoiceJoined { channel, .. } => Some(channel.clone()),
            Event::MemberVoiceLeft { channel, .. } => Some(channel.clone()),
            Event::ClientMoved { channel, .. } => Some(channel.clone()),
            _ => None,
        };
        match related {
            Some(id) => channels.contains(&id),
            // Events unrelated to channels always pass a channel filter.
            None => true,
        }
    }

    pub fn matches(&self, filter: &EventFilter) -> bool {
        if !self.matches_channel(&filter.channels) {
            return false;
        }
        if !filter.exclude.is_empty() && filter.exclude.contains(&self.category()) {
            return false;
        }
        if filter.categories.is_empty() {
            return true;
        }
        filter.categories.contains(&self.category())
    }
}

/// Per-session event bus (cloneable handle).
#[derive(Clone)]
pub struct EventBus {
    tx: broadcast::Sender<Arc<Event>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self { tx }
    }

    /// Publish an event to all subscribers.
    pub fn send(&self, event: Event) {
        // A lagged/absent receiver must not break the publisher.
        let _ = self.tx.send(Arc::new(event));
    }

    /// Subscribe with an optional filter. The returned stream never yields
    /// events rejected by the filter.
    pub fn subscribe(&self, filter: EventFilter) -> EventStream {
        EventStream {
            rx: self.tx.subscribe(),
            filter,
        }
    }

    pub fn subscribe_all(&self) -> EventStream {
        self.subscribe(EventFilter::default())
    }

    /// Register a synchronous callback; runs on a background task that
    /// consumes an internal subscription. The handle aborts on drop.
    pub fn on<F>(&self, filter: EventFilter, mut handler: F) -> CallbackHandle
    where
        F: FnMut(&Event) + Send + 'static,
    {
        let mut stream = self.subscribe(filter);
        let (abort_tx, abort_rx) = tokio::sync::oneshot::channel::<()>();
        let handle = tokio::spawn(async move {
            let mut abort_rx = abort_rx;
            loop {
                tokio::select! {
                    ev = stream.next() => {
                        match ev {
                            Some(ev) => handler(&ev),
                            None => break,
                        }
                    }
                    _ = &mut abort_rx => break,
                }
            }
        });
        CallbackHandle {
            abort: Some(abort_tx),
            task: Some(handle),
        }
    }
}

/// Stream subscription with lag tolerance (a lag resynchronizes silently —
/// the bookkeeping mirror is authoritative for state, events are hints).
pub struct EventStream {
    rx: broadcast::Receiver<Arc<Event>>,
    filter: EventFilter,
}

impl EventStream {
    pub async fn next(&mut self) -> Option<Arc<Event>> {
        loop {
            match self.rx.recv().await {
                Ok(ev) => {
                    if ev.matches(&self.filter) {
                        return Some(ev);
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Map into a `futures::Stream` for interop.
    pub fn into_stream(self) -> impl futures::Stream<Item = Arc<Event>> {
        futures::stream::unfold(self, |mut s| async move {
            s.next().await.map(|ev| (ev, s))
        })
    }
}

/// Aborts the callback task when dropped.
pub struct CallbackHandle {
    abort: Option<tokio::sync::oneshot::Sender<()>>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for CallbackHandle {
    fn drop(&mut self) {
        if let Some(abort) = self.abort.take() {
            let _ = abort.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Channel, DisconnectReason};

    #[tokio::test]
    async fn bus_delivery_and_filtering() {
        let bus = EventBus::new();
        let mut all = bus.subscribe_all();
        let mut voice_only = bus.subscribe(EventFilter {
            categories: vec![Category::Voice],
            ..Default::default()
        });

        bus.send(Event::Connected);
        bus.send(Event::ChannelCreated {
            channel: Channel::default(),
        });
        bus.send(Event::Closed {
            reason: DisconnectReason::Requested { message: None },
        });

        assert!(matches!(all.next().await.as_deref(), Some(Event::Connected)));
        assert!(matches!(
            all.next().await.as_deref(),
            Some(Event::ChannelCreated { .. })
        ));
        assert!(matches!(all.next().await.as_deref(), Some(Event::Closed { .. })));

        // The voice subscriber sees neither Connected nor ChannelCreated.
        tokio::time::timeout(std::time::Duration::from_millis(50), voice_only.next())
            .await
            .unwrap_err();
    }

    #[tokio::test]
    async fn callback_registration() {
        let bus = EventBus::new();
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = counter.clone();
        let handle = bus.on(EventFilter::default(), move |_ev| {
            c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        bus.send(Event::Connected);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        drop(handle); // abort the callback task
        assert!(counter.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    }
}
