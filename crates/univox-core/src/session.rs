//! Session lifecycle and the unified session API (FEATURES.md §2, G1, G5).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;

use crate::bookkeeping::Book;
use crate::connect::{Capabilities, ConnectOptions};
use crate::error::{Error, Result};
use crate::event::{Event, EventBus, EventStream};
use crate::id::{ChannelId, MemberId, MessageId, SessionId};
use crate::message::MessageContent;
use crate::model::{ChannelOptions, ConnectionStats, DisconnectReason, MessageTarget};
use crate::platform::Platform;

/// Lifecycle states (FEATURES.md §2.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Created,
    Connecting,
    Authenticating,
    Handshaking,
    Connected,
    Reconnecting,
    Disconnected,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

pub fn new_session_id() -> SessionId {
    SessionId::from_u64(NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
}

/// Thread-safe state cell shared by session implementations.
#[derive(Debug)]
pub struct StateCell(RwLock<SessionState>);

impl StateCell {
    pub fn new(state: SessionState) -> Self {
        Self(RwLock::new(state))
    }

    pub fn get(&self) -> SessionState {
        *self.0.read().unwrap()
    }

    pub fn set(&self, state: SessionState) {
        *self.0.write().unwrap() = state;
    }

    pub fn is(&self, state: SessionState) -> bool {
        self.get() == state
    }
}

/// Session shared plumbing: state cell + event bus + bookkeeping handle.
/// Drivers embed this and delegate.
pub struct SessionCore {
    pub id: SessionId,
    pub state: StateCell,
    pub bus: EventBus,
    pub book: Book,
    pub capabilities: Capabilities,
    pub stats: RwLock<ConnectionStats>,
    pub tag: RwLock<Option<String>>,
}

impl SessionCore {
    pub fn new(capabilities: Capabilities, book: Book) -> Self {
        Self {
            id: new_session_id(),
            state: StateCell::new(SessionState::Created),
            bus: EventBus::new(),
            book,
            capabilities,
            stats: RwLock::new(ConnectionStats::default()),
            tag: RwLock::new(None),
        }
    }

    pub fn set_state(&self, state: SessionState) {
        self.state.set(state);
        let ev = match state {
            SessionState::Connecting => Event::Connecting,
            SessionState::Authenticating => Event::Authenticating,
            SessionState::Connected => Event::Connected,
            SessionState::Reconnecting => Event::Reconnecting,
            SessionState::Disconnected => Event::Closed {
                reason: DisconnectReason::Requested { message: None },
            },
            SessionState::Created | SessionState::Handshaking => return,
        };
        self.bus.send(ev);
    }

    pub fn update_stats(&self, f: impl FnOnce(&mut ConnectionStats)) {
        if let Ok(mut stats) = self.stats.write() {
            f(&mut stats);
        }
    }
}

/// The unified session API. Methods default to [`Error::Unsupported`] so
/// drivers only implement what their platform supports (G4 capability
/// gating); callers can also consult [`Session::capabilities`] first.
#[async_trait]
pub trait Session: Send + Sync {
    fn id(&self) -> &SessionId;
    fn platform(&self) -> Platform;
    fn state(&self) -> SessionState;
    fn capabilities(&self) -> &Capabilities;
    fn tag(&self) -> Option<String>;
    fn set_tag(&self, tag: Option<String>);

    /// Subscribe to this session's unified events.
    fn events(&self) -> EventStream;

    /// The state mirror (may be disabled via ConnectOptions).
    fn book(&self) -> Book;

    /// Connection quality statistics.
    fn stats(&self) -> ConnectionStats;

    /// Disconnect with an optional leave message. Kicks/bans never
    /// auto-reconnect (FEATURES.md §2.2).
    async fn disconnect(&self, message: Option<String>) -> Result<()>;

    // ---- Messaging (§7) ----

    async fn send_message(
        &self,
        _target: MessageTarget,
        _content: &MessageContent,
    ) -> Result<MessageId> {
        Err(Error::Unsupported("send_message".into()))
    }

    async fn poke(&self, _member: &MemberId, _message: &str) -> Result<()> {
        Err(Error::Unsupported("poke".into()))
    }

    // ---- Voice (§6) ----

    async fn join_voice(&self, _channel: &ChannelId, _password: Option<&str>) -> Result<()> {
        Err(Error::Unsupported("join_voice".into()))
    }

    async fn leave_voice(&self) -> Result<()> {
        Err(Error::Unsupported("leave_voice".into()))
    }

    /// Start sending audio from the source (pull-style PCM).
    async fn start_sending(&self, _source: Box<dyn crate::audio::AudioSource>) -> Result<()> {
        Err(Error::Unsupported("start_sending".into()))
    }

    async fn stop_sending(&self) -> Result<()> {
        Err(Error::Unsupported("stop_sending".into()))
    }

    /// Start receiving mixed audio into the sink.
    async fn start_receiving(&self, _sink: Box<dyn crate::audio::AudioSink>) -> Result<()> {
        Err(Error::Unsupported("start_receiving".into()))
    }

    async fn stop_receiving(&self) -> Result<()> {
        Err(Error::Unsupported("stop_receiving".into()))
    }

    // ---- Channel management (§8) ----

    async fn create_channel(&self, _options: ChannelOptions) -> Result<ChannelId> {
        Err(Error::Unsupported("create_channel".into()))
    }

    async fn edit_channel(&self, _id: &ChannelId, _options: ChannelOptions) -> Result<()> {
        Err(Error::Unsupported("edit_channel".into()))
    }

    async fn delete_channel(&self, _id: &ChannelId, _force: bool) -> Result<()> {
        Err(Error::Unsupported("delete_channel".into()))
    }

    // ---- Member management (§8.3/§9) ----

    async fn move_member(&self, _member: &MemberId, _channel: &ChannelId) -> Result<()> {
        Err(Error::Unsupported("move_member".into()))
    }

    async fn kick_member(
        &self,
        _member: &MemberId,
        _from_channel: bool,
        _reason: Option<&str>,
    ) -> Result<()> {
        Err(Error::Unsupported("kick_member".into()))
    }

    async fn ban_member(&self, _member: &MemberId, _duration: Option<Duration>, _reason: Option<&str>) -> Result<()> {
        Err(Error::Unsupported("ban_member".into()))
    }

    async fn assign_role(&self, _member: &MemberId, _role: &crate::id::RoleId) -> Result<()> {
        Err(Error::Unsupported("assign_role".into()))
    }

    async fn revoke_role(&self, _member: &MemberId, _role: &crate::id::RoleId) -> Result<()> {
        Err(Error::Unsupported("revoke_role".into()))
    }

    // ---- Server (§8.2) ----

    async fn edit_server(&self, _options: &crate::model::Server) -> Result<()> {
        Err(Error::Unsupported("edit_server".into()))
    }

    /// Consume a privilege key / invite token (TS3 privilegekeyuse).
    async fn use_privilege_key(&self, _token: &str) -> Result<()> {
        Err(Error::Unsupported("use_privilege_key".into()))
    }
}

/// Driver factory (G1): one driver per platform.
#[async_trait]
pub trait Driver: Send + Sync {
    fn platform(&self) -> Platform;
    fn capabilities(&self) -> Capabilities;
    async fn connect(&self, opts: ConnectOptions) -> Result<Arc<dyn Session>>;
}

/// Manages any number of cross-platform sessions (G5).
#[derive(Default)]
pub struct SessionManager {
    drivers: RwLock<Vec<Arc<dyn Driver>>>,
    sessions: RwLock<Vec<Arc<dyn Session>>>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register_driver(&self, driver: Arc<dyn Driver>) {
        self.drivers.write().unwrap().push(driver);
    }

    pub fn sessions(&self) -> Vec<Arc<dyn Session>> {
        self.sessions.read().unwrap().clone()
    }

    pub fn find_by_tag(&self, tag: &str) -> Option<Arc<dyn Session>> {
        self.sessions
            .read()
            .unwrap()
            .iter()
            .find(|s| s.tag().as_deref() == Some(tag))
            .cloned()
    }

    /// Connect a session: picks the driver by `opts.platform`, stores the
    /// session under `opts.tag`.
    pub async fn connect(
        &self,
        opts: crate::connect::SessionRequest,
    ) -> Result<Arc<dyn Session>> {
        let driver = self
            .drivers
            .read()
            .unwrap()
            .iter()
            .find(|d| d.platform() == opts.platform)
            .cloned()
            .ok_or_else(|| Error::Unsupported(format!("no driver for {}", opts.platform)))?;
        let session = driver.connect(opts.options).await?;
        session.set_tag(opts.tag.clone());
        self.sessions.write().unwrap().push(session.clone());
        Ok(session)
    }

    /// Drop a session from the registry (after disconnect).
    pub fn remove(&self, id: &SessionId) -> Option<Arc<dyn Session>> {
        let mut sessions = self.sessions.write().unwrap();
        let idx = sessions.iter().position(|s| s.id() == id)?;
        Some(sessions.remove(idx))
    }
}
