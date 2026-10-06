//! The unified [`Ts3Session`]: wires the native client connection into the
//! core abstractions (book mirror, event bus, Session trait, G1/G2/G3).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use univox_core::connect::{Capabilities, ConnectOptions, InitialChannel};
use univox_core::error::{Error, Result};
use univox_core::event::{Event, EventStream};
use univox_core::id::{ChannelId, MemberId, MessageId, SessionId};
use univox_core::message::MessageContent;
use univox_core::model::{ChannelOptions, ConnectionStats, DisconnectReason, MessageTarget};
use univox_core::session::SessionState;
use univox_core::session::{Session, SessionCore};
use univox_core::Book;
use univox_ts3_proto::{Command, Error as T3Error, Identity, RowExt, hash_password};

use crate::book::apply_to_book;
use crate::client::UdpConnection;
use crate::ext::Ts3Ext;

/// Map a protocol error into the unified error type.
pub fn map_proto_err(e: T3Error) -> Error {
    match e {
        T3Error::Server { id, msg, extra } => {
            if id == univox_ts3_proto::error::ids::PERMISSIONS_CLIENT_INSUFFICIENT {
                let missing = extra
                    .iter()
                    .find(|(k, _)| k == "failed_permid")
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                Error::Permission { missing }
            } else {
                Error::Platform {
                    platform: univox_core::platform::Platform::Ts3,
                    code: id,
                    message: msg,
                }
            }
        }
        other => Error::Other(other.to_string()),
    }
}

/// Capability set of the TS3 client driver.
pub fn ts3_capabilities() -> Capabilities {
    Capabilities {
        platform: univox_core::platform::Platform::Ts3,
        audio: univox_core::audio::AudioCapability::FullDuplex,
        message_history: false,
        message_edit: false,
        message_delete: false,
        reactions: false,
        mentions: false,
        channel_management: true,
        server_management: true,
        role_management: true,
        file_transfer: true,
        kicks: true,
        bans: true,
    }
}

/// TS3-specific connect options, attached via
/// [`ConnectOptions::with_extension`](univox_core::connect::ConnectOptions::with_extension).
///
/// Passwords are taken as plaintext and hashed internally; the identity is
/// optionally raised to a target hash-cash security level before the
/// handshake (equivalent to tsclientlib's upgrade-on-connect).
#[derive(Debug, Clone, Default)]
pub struct Ts3ConnectOptions {
    /// Server password (`client_server_password`), plaintext.
    pub server_password: Option<String>,
    /// Privilege key, consumed by the first `clientinit`
    /// (`client_default_token`). Reconnects do not replay it.
    pub privilege_key: Option<String>,
    /// Fail the handshake unless the server's license uid matches this
    /// (anti-DNS-hijack pin).
    pub server_uid_pin: Option<String>,
    /// Raise the identity's hash-cash level to at least this before
    /// connecting (e.g. 24). CPU-bound: runs off the async thread.
    pub upgrade_identity_to: Option<u8>,
}

impl Ts3ConnectOptions {
    /// Look the extension up in unified [`ConnectOptions`].
    fn from_connect(opts: &ConnectOptions) -> Self {
        opts.extension::<Self>().cloned().unwrap_or_default()
    }
}

/// Build the handshake options from the unified connect options plus the
/// TS3 extension.
fn build_handshake_options(
    opts: &ConnectOptions,
    ts3: &Ts3ConnectOptions,
    identity: &Identity,
) -> crate::client::HandshakeOptions {
    let mut hs = crate::client::HandshakeOptions {
        nickname: opts.nickname.clone().unwrap_or_else(|| "UnivoxBot".into()),
        client_key_offset: identity.counter(),
        input_muted: opts.initial_state.input_muted,
        output_muted: opts.initial_state.output_muted,
        ..Default::default()
    };
    match &opts.initial_channel {
        Some(InitialChannel::Path(p)) => hs.default_channel = p.clone(),
        Some(InitialChannel::PathWithPassword { path, password }) => {
            hs.default_channel = path.clone();
            hs.channel_password = hash_password(password);
        }
        // A channel id cannot be carried by clientinit; the session moves
        // there right after connecting.
        Some(InitialChannel::Id(_)) | None => {}
    }
    if let Some(pw) = &ts3.server_password {
        hs.server_password = hash_password(pw);
    }
    if let Some(token) = &ts3.privilege_key {
        hs.default_token = token.clone();
    }
    if let Some(pin) = &ts3.server_uid_pin {
        hs.server_uid_pin = Some(pin.clone());
    }
    hs
}

/// Raise the identity to the target hash-cash level if it is below it.
/// Returns the (possibly upgraded) identity and whether an upgrade
/// happened. CPU-bound work runs off the async thread.
async fn maybe_upgrade_identity(
    identity: Identity,
    target: Option<u8>,
) -> Result<(Identity, bool)> {
    let Some(target) = target else {
        return Ok((identity, false));
    };
    if identity.level() >= target {
        return Ok((identity, false));
    }
    let upgraded =
        tokio::task::spawn_blocking(move || identity.upgrade_level_blocking(target))
            .await
            .map_err(|e| Error::Other(format!("identity upgrade aborted: {e}")))?;
    Ok((upgraded, true))
}

/// A connected TeamSpeak 3 session (native client protocol).
///
/// A supervisor task watches the connection and re-establishes it per the
/// [`ReconnectPolicy`], restoring channel/mute state (FEATURES.md §2.4).
pub struct Ts3Session {
    core: Arc<SessionCore>,
    /// Current connection; swapped by the supervisor on reconnects.
    conn: std::sync::RwLock<Arc<UdpConnection>>,
    addr: std::net::SocketAddr,
    /// Client id assigned by the server; updated on reconnects.
    clid: std::sync::atomic::AtomicU32,
    /// Set when the USER closed the session — the supervisor then stops.
    user_disconnect: std::sync::atomic::AtomicBool,
    /// Active voice send task (FEATURES.md §6.2).
    #[cfg(feature = "voice")]
    sending: std::sync::Mutex<Option<VoiceTask>>,
    /// Active voice receive tasks (FEATURES.md §6.3).
    #[cfg(feature = "voice")]
    receiving: std::sync::Mutex<Option<VoiceTask>>,
    /// Monotonic file-transfer request id (clientftfid).
    ftfid: std::sync::atomic::AtomicU32,
    /// Cached own uid (resolved lazily for avatar management).
    own_uid: std::sync::Mutex<Option<String>>,
    /// Last applied runtime self state, replayed after reconnects
    /// (FEATURES.md §2.4: mutes/away/commander survive a resume).
    pub(crate) last_self_update: std::sync::Mutex<Option<crate::ext::SelfUpdate>>,
    /// Kept for reconnects.
    connect_options: ConnectOptions,
    identity: Identity,
    shutdown: tokio::sync::watch::Sender<bool>,
}

impl Ts3Session {
    /// Connect a new session and start the bookkeeping pump.
    pub async fn connect(opts: ConnectOptions, identity: Identity) -> Result<Arc<Self>> {
        let addr = opts
            .address
            .parse()
            .map_err(|_| Error::InvalidArgument(format!("bad address {}", opts.address)))?;
        let ts3_opts = Ts3ConnectOptions::from_connect(&opts);

        // Auto-upgrade the identity's hash-cash level before the handshake
        // (FEATURES.md §3) — callers don't have to run hash-cash themselves.
        let (identity, upgraded) =
            maybe_upgrade_identity(identity, ts3_opts.upgrade_identity_to).await?;

        let hs_opts = build_handshake_options(&opts, &ts3_opts, &identity);
        let (conn, clid) =
            crate::client::connect(addr, &identity, hs_opts.clone()).await.map_err(map_proto_err)?;

        let capabilities = ts3_capabilities();
        let book = Book::new(univox_core::BookConfig {
            enabled: opts.bookkeeping.enabled,
            member_states: opts.bookkeeping.member_states,
        });
        let core = Arc::new(SessionCore::new(capabilities, book));
        core.set_state(SessionState::Connected);

        let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
        let session = Arc::new(Self {
            core: core.clone(),
            conn: std::sync::RwLock::new(conn.clone()),
            addr,
            clid: std::sync::atomic::AtomicU32::from(u32::from(clid)),
            user_disconnect: std::sync::atomic::AtomicBool::new(false),
            #[cfg(feature = "voice")]
            sending: std::sync::Mutex::new(None),
            #[cfg(feature = "voice")]
            receiving: std::sync::Mutex::new(None),
            ftfid: std::sync::atomic::AtomicU32::new(1),
            own_uid: std::sync::Mutex::new(None),
            last_self_update: std::sync::Mutex::new(None),
            connect_options: opts,
            identity,
            shutdown: shutdown_tx,
        });

        start_pump(&core, &conn, clid, shutdown_rx.clone());
        prime_book(&core, &conn, clid).await;

        // The privilege key was consumed by this connect; reconnects must
        // not replay it. Passwords stay — the server still wants them.
        let mut reconnect_hs = hs_opts;
        reconnect_hs.default_token.clear();

        // The supervisor: watch for connection loss and reconnect per the
        // policy, restoring state (FEATURES.md §2.2/§2.4).
        let sup = session.clone();
        tokio::spawn(async move {
            sup.supervise(reconnect_hs, shutdown_rx).await;
        });

        // A channel id cannot ride on clientinit — join it explicitly.
        if let Some(InitialChannel::Id(cid)) = &session.connect_options.initial_channel {
            let me = MemberId::from_u64(u64::from(clid));
            if let Err(e) = session.move_member(&me, cid).await {
                tracing::warn!(
                    error = %e,
                    channel = cid.as_u64().unwrap_or(0),
                    "initial channel join failed"
                );
            }
        }
        if upgraded {
            session.core.bus.send(Event::IdentityLevelIncreased {
                level: u32::from(session.identity.level()),
            });
        }

        Ok(session)
    }

    pub fn conn(&self) -> Arc<UdpConnection> {
        self.conn.read().unwrap().clone()
    }

    pub(crate) fn clid(&self) -> u16 {
        self.clid.load(std::sync::atomic::Ordering::Relaxed) as u16
    }

    /// Reconnect loop: wait for the connection to die, then re-establish it
    /// until the policy is exhausted or the user disconnected.
    async fn supervise(
        self: Arc<Self>,
        hs_opts: crate::client::HandshakeOptions,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) {
        loop {
            let conn = self.conn();
            tokio::select! {
                _ = conn.wait_closed() => {}
                _ = shutdown.changed() => break,
            }
            if self.user_disconnect.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }

            let reason = conn
                .close_reason()
                .map(DisconnectReason::Other)
                .unwrap_or(DisconnectReason::Network("connection lost".into()));
            self.core.bus.send(Event::TemporarilyDisconnected { reason });
            // Where we were before the drop, for the state restore.
            let old_channel = self
                .core
                .book
                .with(|b| b.self_member.channel_id.clone())
                .unwrap_or(None);
            // Stale mirror contents (members gone while we were offline).
            self.core.book.clear();
            self.core.set_state(SessionState::Reconnecting);

            let policy = &self.connect_options.reconnect;
            let mut reconnected = None;
            for attempt in 0..policy.max_attempts {
                tokio::time::sleep(policy.delay_for(attempt)).await;
                if *shutdown.borrow() {
                    break;
                }
                // First try keeps the user's identity (server restarts forget
                // it); later tries use a fresh one — after a network blip the
                // server briefly rejects the still-registered clone.
                let attempt_identity = if attempt == 0 {
                    self.identity.clone()
                } else {
                    Identity::create()
                };
                match crate::client::spawn_once(self.addr, &attempt_identity, hs_opts.clone())
                    .await
                {
                    Ok(pair) => {
                        reconnected = Some(pair);
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(attempt, error = %e, "reconnect attempt failed");
                        // The server flood-bans misbehaving clients; back off
                        // fully before the next attempt in that case.
                        if e.to_string().contains("flooding") {
                            tokio::time::sleep(policy.max_delay).await;
                        }
                    }
                }
            }

            let Some((new_conn, new_clid)) = reconnected else {
                self.core.state.set(SessionState::Disconnected);
                self.core.bus.send(Event::Closed {
                    reason: DisconnectReason::Network(
                        "reconnect attempts exhausted".into(),
                    ),
                });
                break;
            };

            *self.conn.write().unwrap() = new_conn.clone();
            self.clid
                .store(u32::from(new_clid), std::sync::atomic::Ordering::Relaxed);
            start_pump(&self.core, &new_conn, new_clid, shutdown.clone());
            prime_book(&self.core, &new_conn, new_clid).await;

            if policy.restore_state {
                if let Some(channel) = old_channel {
                    let _ = new_conn
                        .exec(
                            Command::new("clientmove")
                                .param("clid", new_clid)
                                .param("cid", channel.as_u64().unwrap_or(0)),
                        )
                        .await;
                }
                // Replay the last runtime self state (clientupdate is
                // idempotent); input/output muted additionally ride every
                // clientinit via the reused handshake options.
                let replay = self.last_self_update.lock().unwrap().clone();
                if let Some(update) = replay {
                    let _ = new_conn.exec(update.into_command()).await;
                }
            }

            self.core.update_stats(|s| s.reconnect_count += 1);
            self.core.set_state(SessionState::Connected);
            // set_state emits the generic Connected event; the resumed
            // session additionally reports Reconnected (FEATURES.md §2.4).
            self.core.bus.send(Event::Reconnected);
        }
    }

    /// Raw command escape hatch: execute a command and return its rows.
    pub async fn exec(&self, cmd: Command) -> Result<crate::client::Rows> {
        self.conn().exec(cmd).await.map_err(map_proto_err)
    }

    /// The identity this session authenticated with — after any
    /// `Ts3ConnectOptions::upgrade_identity_to` work, so `counter()` and
    /// `max_counter()` reflect what this connection actually used.
    ///
    /// Persist them after connecting: the hash-cash search resumes from
    /// `max_counter`, and a later session that starts from a smaller
    /// counter wastes work (and may fall below a server that raised its
    /// required security level).
    pub fn identity(&self) -> &Identity {
        &self.identity
    }

    /// Run a command, discarding its response rows.
    pub(crate) async fn exec_ok(&self, cmd: Command) -> Result<()> {
        self.exec(cmd).await.map(|_| ())
    }

    /// This session's own client unique identifier (cached after connect).
    pub async fn own_uid(&self) -> Result<String> {
        if let Some(uid) = self.own_uid.lock().unwrap().clone() {
            return Ok(uid);
        }
        let me = MemberId::from_u64(u64::from(self.clid()));
        let uid = self.uid_from_clid(&me).await?;
        *self.own_uid.lock().unwrap() = Some(uid.clone());
        Ok(uid)
    }

    /// Next clientftfid for file-transfer negotiations.
    pub fn next_ftfid(&self) -> u32 {
        self.ftfid
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    #[cfg(feature = "voice")]
    fn stop_task(lock: &std::sync::Mutex<Option<VoiceTask>>) {
        if let Some(task) = lock.lock().unwrap().take() {
            let _ = task.stop.send(true);
            task.handle.abort();
        }
    }
}

/// A cancellable spawned voice task.
#[cfg(feature = "voice")]
struct VoiceTask {
    stop: tokio::sync::watch::Sender<bool>,
    handle: tokio::task::JoinHandle<()>,
}

/// The send pipeline: pull PCM from the source in 20 ms frames, Opus-encode
/// and ship as TS3 voice packets (FEATURES.md §6.2).
#[cfg(feature = "voice")]
async fn run_send_pipeline(
    conn: Arc<UdpConnection>,
    mut source: Box<dyn univox_core::audio::AudioSource>,
    codec: std::sync::Arc<univox_voice::OpusEncoder>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(
        univox_voice::FRAME_MS as u64,
    ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            _ = stop.changed() => break,
        }
        let mut frame = [0.0f32; univox_voice::FRAME_SAMPLES];
        if source.read(&mut frame).await.unwrap_or(0) == 0 {
            continue;
        }
        match codec.encode(&frame) {
            Ok(packet) => {
                // Content = codec byte + opus payload; the actor prepends
                // the voice sequence id.
                let mut content = Vec::with_capacity(1 + packet.len());
                content.push(univox_voice::CODEC_OPUS_VOICE);
                content.extend_from_slice(&packet);
                conn.send_voice(content, univox_ts3_proto::PacketType::Voice).await;
            }
            Err(e) => tracing::warn!(error = %e, "voice encode failed"),
        }
    }
}

/// The receive pipeline: decode incoming voice packets into per-member
/// jitter buffers, mix and push PCM to the sink; emits Speaking events
/// (FEATURES.md §6.4).
#[cfg(feature = "voice")]
async fn run_recv_pipeline(
    conn: Arc<UdpConnection>,
    mut sink: Box<dyn univox_core::audio::AudioSink>,
    codec: std::sync::Arc<univox_voice::OpusDecoder>,
    core: Arc<SessionCore>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) {
    let mut voice_rx = conn.voice_sink_handle().subscribe();
    let mixer = std::sync::Arc::new(std::sync::Mutex::new(univox_voice::Mixer::new(5)));
    let mixer_decode = mixer.clone();

    // Decode task.
    let mut stop2 = stop.clone();
    let decoder = tokio::spawn(async move {
        loop {
            let packet = tokio::select! {
                p = voice_rx.recv() => p,
                _ = stop2.changed() => break,
            };
            let Ok(packet) = packet else { break };
            // Whispered audio arrives as S2CWhisper but carries the same
            // shape — mix it like normal voice.
            let (id, from, c, data) = match packet {
                univox_ts3_proto::VoiceData::S2C { id, from, codec, data }
                | univox_ts3_proto::VoiceData::S2CWhisper { id, from, codec, data } => {
                    (id, from, codec, data)
                }
                _ => continue,
            };
            if c != univox_voice::CODEC_OPUS_VOICE {
                continue;
            }
            if let Ok(frame) = codec.decode(Some(&data)) {
                mixer_decode
                    .lock()
                    .unwrap()
                    .push(MemberId::from_u64(u64::from(from)), id, frame);
            }
        }
    });

    // Mixer/pacer task: emits one mixed frame every 20 ms plus speaking
    // transitions to the event bus.
    let pacer = tokio::spawn(async move {
        let mut speaking: Vec<MemberId> = Vec::new();
        let mut ticker = tokio::time::interval(std::time::Duration::from_millis(
            univox_voice::FRAME_MS as u64,
        ));
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = ticker.tick() => {}
                _ = stop.changed() => break,
            }
            let (mixed, contributors) = mixer.lock().unwrap().mix_frame(univox_voice::FRAME_SAMPLES);
            for m in &contributors {
                if !speaking.contains(m) {
                    core.bus.send(Event::SpeakingStarted { member: m.clone() });
                }
            }
            for m in &speaking {
                if !contributors.contains(m) {
                    core.bus.send(Event::SpeakingStopped { member: m.clone() });
                }
            }
            speaking = contributors;
            if let Err(e) = sink
                .write(univox_core::audio::AudioPacket { member: None, samples: mixed })
                .await
            {
                tracing::warn!(error = %e, "audio sink failed; stopping receive pipeline");
                break;
            }
        }
    });
    let _ = decoder.await;
    let _ = pacer.await;
}

/// Pump: client-protocol notifications → book mirror + unified events.
fn start_pump(
    core: &Arc<SessionCore>,
    conn: &Arc<UdpConnection>,
    clid: u16,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let pump_core = core.clone();
    let pump_clid = u64::from(clid);
    let mut notifications = conn.subscribe();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                cmd = notifications.recv() => {
                    let Some(cmd) = cmd else { break };
                    let events = apply_to_book(&pump_core.book, pump_clid, &cmd, crate::book::StreamOrigin::Client);
                    for ev in events {
                        pump_core.bus.send(ev);
                    }
                }
                _ = shutdown.changed() => break,
            }
        }
    });
}

/// Fill the book from a full-sync dump (no events — initial state, not a
/// transition).
fn apply_dump(core: &SessionCore, name: &str, rows: crate::client::Rows, clid: u16) {
    if name == "clientlist" {
        let self_clid_str = clid.to_string();
        let self_row = rows
            .iter()
            .find(|r| r.iter().any(|(k, v)| k == "clid" && *v == self_clid_str));
        let self_cid = self_row.and_then(|r| {
            r.iter()
                .find(|(k, _)| k == "cid")
                .map(|(_, v)| v.clone())
        });
        tracing::info!(
            rows = rows.len(),
            self_present = self_row.is_some(),
            self_cid = ?self_cid,
            "clientlist dump"
        );
    }
    let mut dump = Command::new(name);
    dump.params = rows;
    for ev in apply_to_book(&core.book, u64::from(clid), &dump, crate::book::StreamOrigin::Client) {
        core.bus.send(ev);
    }
}

/// Request the full client roster once, like the official client does (the
/// server pushes `channellist` by itself, but not `clientlist`). Some server
/// groups (guests) lack the view permission — ignore that failure; members
/// still arrive via `notifycliententerview` as they connect.
async fn prime_book(core: &Arc<SessionCore>, conn: &Arc<UdpConnection>, clid: u16) {
    if !core.book.enabled() {
        return;
    }
    match conn
        .exec(
            Command::new("clientlist")
                .opt("uid")
                .opt("away")
                .opt("voice")
                .opt("groups"),
        )
        .await
    {
        Ok(rows) => apply_dump(core, "clientlist", rows, clid),
        // Permission-restricted servers (plain guests) deny the bulk dump —
        // the roster then only grows with own-channel occupants. Make the
        // denial visible instead of silently degrading.
        Err(e) => tracing::warn!(
            error = %e,
            "clientlist dump denied; roster limited to own-channel occupants"
        ),
    }
}

#[async_trait]
impl Session for Ts3Session {
    fn id(&self) -> &SessionId {
        &self.core.id
    }

    fn platform(&self) -> univox_core::platform::Platform {
        univox_core::platform::Platform::Ts3
    }

    fn state(&self) -> SessionState {
        self.core.state.get()
    }

    fn capabilities(&self) -> &Capabilities {
        &self.core.capabilities
    }

    fn tag(&self) -> Option<String> {
        self.core.tag.read().unwrap().clone()
    }

    fn set_tag(&self, tag: Option<String>) {
        *self.core.tag.write().unwrap() = tag;
    }

    fn events(&self) -> EventStream {
        self.core.bus.subscribe_all()
    }

    fn book(&self) -> Book {
        self.core.book.clone()
    }

    fn stats(&self) -> ConnectionStats {
        let mut s = self.conn().stats_snapshot();
        s.reconnect_count = self.core.stats.read().unwrap().reconnect_count;
        s
    }

    async fn disconnect(&self, message: Option<String>) -> Result<()> {
        self.user_disconnect
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.core.set_state(SessionState::Disconnected);
        self.conn()
            .disconnect(1, message.as_deref().unwrap_or("disconnecting"))
            .await;
        let _ = self.shutdown.send(true);
        Ok(())
    }

    async fn send_message(
        &self,
        target: MessageTarget,
        content: &MessageContent,
    ) -> Result<MessageId> {
        let text = content.plain_text();
        let cmd = crate::book::sendtextmessage_command(&target, &text).map_err(map_proto_err)?;
        self.exec(cmd).await?;
        Ok(MessageId::from_string(format!(
            "ts3-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        )))
    }

    async fn poke(&self, member: &MemberId, message: &str) -> Result<()> {
        self.exec_ok(
            Command::new("clientpoke")
                .param("clid", member.as_u64().unwrap_or(0))
                .param("msg", message),
        )
        .await
    }

    async fn join_voice(&self, channel: &ChannelId, password: Option<&str>) -> Result<()> {
        // clientmove carries an optional `cpw` (base64(sha1)) for
        // password-protected target channels.
        let mut cmd = Command::new("clientmove")
            .param("clid", u64::from(self.clid()))
            .param("cid", channel.as_u64().unwrap_or(0));
        if let Some(pw) = password {
            cmd = cmd.param("cpw", hash_password(pw));
        }
        self.exec_ok(cmd).await
    }

    async fn leave_voice(&self) -> Result<()> {
        Err(Error::Unsupported(
            "TS3 clients always remain in a channel".into(),
        ))
    }

    #[cfg(feature = "voice")]
    async fn start_sending(&self, source: Box<dyn univox_core::audio::AudioSource>) -> Result<()> {
        Self::stop_task(&self.sending);
        let codec = std::sync::Arc::new(univox_voice::OpusEncoder::new()?);
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let handle = tokio::spawn(run_send_pipeline(
            self.conn(),
            source,
            codec,
            stop_rx,
        ));
        *self.sending.lock().unwrap() = Some(VoiceTask { stop: stop_tx, handle });
        Ok(())
    }

    #[cfg(not(feature = "voice"))]
    async fn start_sending(
        &self,
        _source: Box<dyn univox_core::audio::AudioSource>,
    ) -> Result<()> {
        Err(Error::Unsupported(
            "univox-ts3 built without the `voice` feature".into(),
        ))
    }

    #[cfg(feature = "voice")]
    async fn stop_sending(&self) -> Result<()> {
        Self::stop_task(&self.sending);
        Ok(())
    }

    #[cfg(not(feature = "voice"))]
    async fn stop_sending(&self) -> Result<()> {
        Ok(())
    }

    #[cfg(feature = "voice")]
    async fn start_receiving(&self, sink: Box<dyn univox_core::audio::AudioSink>) -> Result<()> {
        Self::stop_task(&self.receiving);
        let codec = std::sync::Arc::new(univox_voice::OpusDecoder::new()?);
        let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
        let conn = self.conn();
        let core = self.core.clone();
        let handle = tokio::spawn(run_recv_pipeline(conn, sink, codec, core, stop_rx));
        *self.receiving.lock().unwrap() = Some(VoiceTask { stop: stop_tx, handle });
        Ok(())
    }

    #[cfg(not(feature = "voice"))]
    async fn start_receiving(&self, _sink: Box<dyn univox_core::audio::AudioSink>) -> Result<()> {
        Err(Error::Unsupported(
            "univox-ts3 built without the `voice` feature".into(),
        ))
    }

    #[cfg(feature = "voice")]
    async fn stop_receiving(&self) -> Result<()> {
        Self::stop_task(&self.receiving);
        Ok(())
    }

    #[cfg(not(feature = "voice"))]
    async fn stop_receiving(&self) -> Result<()> {
        Ok(())
    }

    async fn create_channel(&self, options: ChannelOptions) -> Result<ChannelId> {
        let mut cmd = Command::new("channelcreate").param("channel_name", &options.name);
        if let Some(parent) = &options.parent {
            cmd = cmd.param("cpid", parent.as_u64().unwrap_or(0));
        }
        match options.permanence {
            univox_core::model::Permanence::Permanent => {
                cmd = cmd.param("channel_flag_permanent", 1);
            }
            univox_core::model::Permanence::SemiPermanent => {
                cmd = cmd.param("channel_flag_semi_permanent", 1);
            }
            univox_core::model::Permanence::Temporary => {
                cmd = cmd.param("channel_flag_temporary", 1);
            }
        }
        if let Some(limit) = options.user_limit {
            cmd = cmd.param("channel_maxclients", limit);
        }
        if options.default_channel {
            cmd = cmd.param("channel_flag_default", 1);
        }
        if let Some(pw) = &options.password {
            cmd = cmd.param("channel_password", pw);
        }
        // Generic passthrough: `extra` carries raw protocol params the
        // typed fields don't model (channel_order,
        // channel_needed_talk_power, channel_icon_id, ...).
        for (k, v) in &options.extra {
            cmd = cmd.param(k.as_str(), v.as_str());
        }
        let rows = self.exec(cmd).await?;
        // The server sends no data row for channelcreate; it announces the
        // channel to us (and everyone) via `notifychannelcreated`, which the
        // pump mirrors into the book. Wait for that mirror (bounded).
        let mut cid = rows
            .first()
            .and_then(|r| r.get("cid"))
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|v| *v > 0);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while cid.is_none() && tokio::time::Instant::now() < deadline {
            cid = self
                .book()
                .channels()
                .iter()
                .filter(|c| c.name == options.name)
                .map(|c| c.id.as_u64().unwrap_or(0))
                .max()
                .filter(|v| *v > 0);
            if cid.is_none() {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
        let cid = cid.ok_or_else(|| Error::Other("channelcreate: no cid".into()))?;
        Ok(ChannelId::from_u64(cid))
    }

    async fn edit_channel(&self, id: &ChannelId, options: ChannelOptions) -> Result<()> {
        let mut cmd = Command::new("channeledit").param("cid", id.as_u64().unwrap_or(0));
        if !options.name.is_empty() {
            cmd = cmd.param("channel_name", &options.name);
        }
        if let Some(topic) = &options.topic {
            cmd = cmd.param("channel_topic", topic);
        }
        if let Some(desc) = &options.description {
            cmd = cmd.param("channel_description", desc);
        }
        if let Some(limit) = options.user_limit {
            cmd = cmd.param("channel_maxclients", limit);
        }
        // Generic passthrough (see create_channel): e.g. channel_order and
        // channel_needed_talk_power for the two edit-only knobs.
        for (k, v) in &options.extra {
            cmd = cmd.param(k.as_str(), v.as_str());
        }
        self.exec_ok(cmd).await
    }

    async fn delete_channel(&self, id: &ChannelId, force: bool) -> Result<()> {
        self.exec_ok(
            Command::new("channeldelete")
                .param("cid", id.as_u64().unwrap_or(0))
                .param("force", u8::from(force)),
        )
        .await
    }

    async fn move_member(&self, member: &MemberId, channel: &ChannelId) -> Result<()> {
        self.exec_ok(
            Command::new("clientmove")
                .param("clid", member.as_u64().unwrap_or(0))
                .param("cid", channel.as_u64().unwrap_or(0)),
        )
        .await
    }

    async fn kick_member(
        &self,
        member: &MemberId,
        from_channel: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        let mut cmd = Command::new("clientkick")
            .param("clid", member.as_u64().unwrap_or(0))
            .param("reasonid", u8::from(!from_channel) + 4);
        if let Some(r) = reason {
            cmd = cmd.param("reasonmsg", r);
        }
        self.exec_ok(cmd).await
    }

    async fn ban_member(
        &self,
        member: &MemberId,
        duration: Option<Duration>,
        reason: Option<&str>,
    ) -> Result<()> {
        // Ban by the member's current identity data via banclient.
        let mut cmd = Command::new("banclient").param("clid", member.as_u64().unwrap_or(0));
        if let Some(d) = duration {
            cmd = cmd.param("time", d.as_secs());
        }
        if let Some(r) = reason {
            cmd = cmd.param("banreason", r);
        }
        self.exec_ok(cmd).await
    }

    async fn assign_role(&self, member: &MemberId, role: &univox_core::id::RoleId) -> Result<()> {
        self.exec_ok(
            Command::new("servergroupaddclient")
                .param("sgid", role.as_u64().unwrap_or(0))
                .param("cldbid", member.as_u64().unwrap_or(0)),
        )
        .await
    }

    async fn revoke_role(&self, member: &MemberId, role: &univox_core::id::RoleId) -> Result<()> {
        self.exec_ok(
            Command::new("servergroupdelclient")
                .param("sgid", role.as_u64().unwrap_or(0))
                .param("cldbid", member.as_u64().unwrap_or(0)),
        )
        .await
    }

    async fn use_privilege_key(&self, token: &str) -> Result<()> {
        self.exec_ok(Command::new("privilegekeyuse").param("token", token)).await
    }
}

/// The TS3 driver (G1): creates client-protocol sessions.
pub struct Ts3Driver;

#[async_trait]
impl univox_core::session::Driver for Ts3Driver {
    fn platform(&self) -> univox_core::platform::Platform {
        univox_core::platform::Platform::Ts3
    }

    fn capabilities(&self) -> Capabilities {
        ts3_capabilities()
    }

    async fn connect(&self, opts: ConnectOptions) -> Result<Arc<dyn Session>> {
        let identity = match &opts.credential {
            Some(univox_core::credential::Credential::Ts3Identity { identity }) => {
                Identity::parse(identity).map_err(map_proto_err)?
            }
            _ => Identity::create(),
        };
        // Parse invite links and resolve ports (SRV/TSDNS) for bare hosts.
        let mut opts = opts;
        if let Ok(addr) = crate::address::parse(&opts.address) {
            if !addr.port_explicit {
                let path = addr.channel.as_deref().unwrap_or("");
                if let Ok(port) =
                    crate::address::resolve_port(&addr.host, path, crate::address::DEFAULT_TSDNS_PORT)
                        .await
                {
                    opts.address = format!("{}:{port}", addr.host);
                }
            } else {
                opts.address = format!("{}:{}", addr.host, addr.port);
            }
            if opts.nickname.is_none() {
                if let Some(nick) = &addr.nickname {
                    opts.nickname = Some(nick.clone());
                }
            }
            // An invite link carrying a channel joins it after connecting.
            if opts.initial_channel.is_none() {
                if let Some(channel) = addr.channel.filter(|c| !c.is_empty()) {
                    opts.initial_channel = Some(InitialChannel::Path(channel));
                }
            }
        }
        Ts3Session::connect(opts, identity).await.map(|s| s as Arc<dyn Session>)
    }
}

/// The client id assigned to this session by the server.
pub fn self_clid(session: &Ts3Session) -> u64 {
    u64::from(session.clid())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connect_opts(ts3: Ts3ConnectOptions) -> ConnectOptions {
        ConnectOptions::new("127.0.0.1:9987").with_extension(ts3)
    }

    #[test]
    fn handshake_options_wiring() {
        // Plaintext passwords are hashed internally; privilege key and uid
        // pin pass through; the initial channel feeds the default channel.
        let opts = connect_opts(Ts3ConnectOptions {
            server_password: Some("s3cret".into()),
            privilege_key: Some("token123".into()),
            server_uid_pin: Some("testuid=".into()),
            upgrade_identity_to: None,
        })
        .nickname("Bot")
        .initial_channel(InitialChannel::PathWithPassword {
            path: "/lobby/hall".into(),
            password: "chpw".into(),
        });
        let identity = Identity::create();
        let ts3 = Ts3ConnectOptions::from_connect(&opts);
        let hs = build_handshake_options(&opts, &ts3, &identity);
        assert_eq!(hs.nickname, "Bot");
        assert_eq!(hs.default_channel, "/lobby/hall");
        assert_eq!(hs.channel_password, hash_password("chpw"));
        assert_eq!(hs.server_password, hash_password("s3cret"));
        assert_eq!(hs.default_token, "token123");
        assert_eq!(hs.server_uid_pin.as_deref(), Some("testuid="));
        assert_eq!(hs.client_key_offset, identity.counter());
    }

    #[test]
    fn handshake_options_without_extension() {
        let opts = ConnectOptions::new("127.0.0.1:9987");
        let hs = build_handshake_options(&opts, &Ts3ConnectOptions::default(), &Identity::create());
        assert_eq!(hs.default_channel, "");
        assert_eq!(hs.channel_password, "");
        assert_eq!(hs.server_password, "");
        assert_eq!(hs.default_token, "");
        assert!(hs.server_uid_pin.is_none());
    }

    #[tokio::test]
    async fn identity_upgrade_targets_level() {
        // A fresh counter-0 identity is below level 12 and gets raised.
        let identity = Identity::new(univox_ts3_proto::EccKeyPrivP256::create(), 0);
        let (identity, upgraded) =
            maybe_upgrade_identity(identity, Some(12)).await.unwrap();
        assert!(upgraded);
        assert!(identity.level() >= 12);

        // No target / already-sufficient level: unchanged.
        let (identity, upgraded) = maybe_upgrade_identity(identity, None).await.unwrap();
        assert!(!upgraded);
        let (_, upgraded) =
            maybe_upgrade_identity(identity, Some(1)).await.unwrap();
        assert!(!upgraded);
    }
}
