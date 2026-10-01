//! Client protocol connection: a UDP actor that performs the TS3 handshake
//! (Init1 → initivexpand2 → clientek → clientinit → initserver) internally
//! and then serves commands, notifications and voice.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::{broadcast, mpsc, oneshot};

use univox_ts3_proto as proto;
use univox_ts3_proto::Result;
use proto::{Command, Direction, Error, Flags, Header, PacketType, S2CInitData, VoiceData};

/// How long to wait for each handshake step.
const HANDSHAKE_STEP_TIMEOUT: Duration = Duration::from_secs(6);
/// TS3 offsets the version timestamp by this many seconds.
const VERSION_TS_OFFSET: u64 = 1_356_998_400;

/// Options for the handshake phase.
#[derive(Clone)]
pub struct HandshakeOptions {
    pub nickname: String,
    /// `client_version` string.
    pub version: String,
    /// `client_platform` string.
    pub platform: String,
    /// base64 version signature (may be empty).
    pub version_sign: String,
    /// Identity hash-cash counter (`client_key_offset`).
    pub client_key_offset: u64,
    /// base64 tomcrypt DER of the identity *public* key (Init4 `omega`).
    pub omega: Vec<u8>,
    pub input_muted: bool,
    pub output_muted: bool,
    pub default_channel: String,
    /// base64(sha1(password)) or empty.
    pub channel_password: String,
    pub server_password: String,
    /// Verify the server's identity uid against this (anti-DNS-hijack).
    pub server_uid_pin: Option<String>,
}

impl Default for HandshakeOptions {
    fn default() -> Self {
        Self {
            nickname: "UnivoxBot".into(),
            // Recent TS3 clients hide their version; the server accepts this
            // string with its official signature (ReSpeak tsdeclarations
            // Versions.csv).
            version: "3.?.? [Build: 5680278000]".into(),
            platform: "Linux".into(),
            version_sign: "Hjd+N58Gv3ENhoKmGYy2bNRBsNNgm5kpiaQWxOj5HN2DXttG6REjymSwJtpJ8muC2gSwRuZi0R+8Laan5ts5CQ==".into(),
            client_key_offset: 0,
            omega: Vec::new(),
            input_muted: true,
            output_muted: false,
            default_channel: String::new(),
            channel_password: String::new(),
            server_password: String::new(),
            server_uid_pin: None,
        }
    }
}

/// Connection parameters established by the handshake.
pub(crate) struct HandshakeResult {
    pub params: Params,
    /// Our assigned client id (from `initserver aclid`).
    pub clid: u16,
    /// The server's P-256 public key (uid pin verification).
    pub server_key: proto::EccKeyPubP256,
}

/// Crypto parameters of an established connection.
#[derive(Clone)]
pub(crate) struct Params {
    pub c_id: u16,
    pub voice_encryption: bool,
    pub shared_iv: [u8; 64],
    pub shared_mac: [u8; 8],
    pub key_cache: proto::KeyCache,
}

struct PendingOut {
    datagrams: VecDeque<Vec<u8>>,
    p_type: PacketType,
    p_id: u16,
    last_sent: Instant,
    resends: u32,
}

enum Request {
    Exec {
        cmd: Command,
        reply: oneshot::Sender<Result<Rows>>,
    },
    SendVoice {
        content: Vec<u8>,
        p_type: PacketType,
    },
    SetVoiceSink {
        tx: broadcast::Sender<VoiceData>,
    },
    Disconnect {
        reasonid: u8,
        reasonmsg: String,
    },
}

pub type Rows = Vec<Vec<(String, String)>>;

/// Handle to the connected client actor.
pub struct UdpConnection {
    pub addr: SocketAddr,
    tx: mpsc::Sender<Request>,
    notify_tx: broadcast::Sender<Command>,
    voice_tx: broadcast::Sender<VoiceData>,
    closed: std::sync::Arc<tokio::sync::watch::Receiver<bool>>,
}

impl UdpConnection {
    /// Spawn the actor and run the handshake. Resolves once the connection
    /// is established (initserver received) or the handshake failed.
    pub async fn spawn(
        sock: UdpSocket,
        addr: SocketAddr,
        opts: HandshakeOptions,
        private_key: proto::EccKeyPrivP256,
    ) -> Result<Self> {
        let (notify_tx, _) = broadcast::channel(256);
        let (voice_tx, _) = broadcast::channel(256);
        let (req_tx, req_rx) = mpsc::channel(128);
        let (closed_tx, closed_rx) = tokio::sync::watch::channel(false);
        let (done_tx, done_rx): (oneshot::Sender<Result<HandshakeResult>>, _) = oneshot::channel();

        tokio::spawn(run_actor(
            sock,
            addr,
            opts,
            private_key,
            req_rx,
            notify_tx.clone(),
            voice_tx.clone(),
            closed_tx,
            done_tx,
        ));

        // Wait for the handshake result.
        match tokio::time::timeout(Duration::from_secs(15), done_rx).await {
            Ok(Ok(Ok(handshake))) => {
                // Push the params into the actor's request channel.
                let conn = Self {
                    addr,
                    tx: req_tx,
                    notify_tx,
                    voice_tx,
                    closed: std::sync::Arc::new(closed_rx),
                };
                // The actor already stored its own params; nothing to send.
                let _ = handshake;
                Ok(conn)
            }
            Ok(Ok(Err(e))) => Err(e),
            Ok(Err(_)) => Err(Error::Closed),
            Err(_) => Err(Error::Timeout),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Command> {
        self.notify_tx.subscribe()
    }

    pub fn voice_sink_handle(&self) -> broadcast::Sender<VoiceData> {
        self.voice_tx.clone()
    }

    pub fn is_closed(&self) -> bool {
        *self.closed.borrow()
    }

    pub async fn exec(&self, cmd: Command) -> Result<Rows> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(Request::Exec { cmd, reply: reply_tx })
            .await
            .map_err(|_| Error::Closed)?;
        match tokio::time::timeout(Duration::from_secs(20), reply_rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::Closed),
            Err(_) => Err(Error::Timeout),
        }
    }

    pub async fn send_voice(&self, content: Vec<u8>, p_type: PacketType) {
        let _ = self
            .tx
            .send(Request::SendVoice { content, p_type })
            .await;
    }

    pub async fn set_voice_sink(&self, tx: broadcast::Sender<VoiceData>) {
        let _ = self.tx.send(Request::SetVoiceSink { tx }).await;
    }

    pub async fn disconnect(&self, reasonid: u8, reasonmsg: &str) {
        let _ = self
            .tx
            .send(Request::Disconnect {
                reasonid,
                reasonmsg: reasonmsg.to_string(),
            })
            .await;
    }
}

struct Codec {
    outgoing_p_ids: [u16; proto::PACKET_TYPE_COUNT],
    outgoing_generations: [u32; proto::PACKET_TYPE_COUNT],
    incoming_p_ids: [u16; proto::PACKET_TYPE_COUNT],
    incoming_command_generation: u32,
    receive_queue: [VecDeque<Vec<u8>>; 2],
    fragmented_queue: [Option<Vec<u8>>; 2],
}

impl Codec {
    fn new() -> Self {
        let mut ids = [0u16; proto::PACKET_TYPE_COUNT];
        // The clientinitiv command embedded in Init4 consumes Command id 0;
        // the first real command (clientek) uses id 1.
        ids[PacketType::Command as usize] = 1;
        Self {
            outgoing_p_ids: ids,
            outgoing_generations: [0; proto::PACKET_TYPE_COUNT],
            incoming_p_ids: [0; proto::PACKET_TYPE_COUNT],
            incoming_command_generation: 0,
            receive_queue: Default::default(),
            fragmented_queue: [None, None],
        }
    }

    fn next_out(&mut self, p_type: PacketType) -> u16 {
        let t = p_type as usize;
        let id = self.outgoing_p_ids[t];
        let (next, wrapped) = id.overflowing_add(1);
        self.outgoing_p_ids[t] = next;
        if wrapped {
            self.outgoing_generations[t] += 1;
        }
        id
    }

    fn out_generation(&self, p_type: PacketType) -> u32 {
        self.outgoing_generations[p_type as usize]
    }

    fn in_receive_window(&self, p_type: PacketType, p_id: u16) -> bool {
        let cur_next = self.incoming_p_ids[p_type as usize];
        let (limit, wrapped) = cur_next.overflowing_add(u16::MAX / 2);
        (!wrapped && p_id >= cur_next && p_id < limit)
            || (wrapped && (p_id >= cur_next || p_id < limit))
    }

    fn advance_incoming(&mut self, p_type: PacketType, p_id: u16) {
        let (next, wrapped) = p_id.overflowing_add(1);
        self.incoming_p_ids[p_type as usize] = next;
        if wrapped && p_type.is_command() {
            self.incoming_command_generation += 1;
        }
    }
}

struct Actor {
    sock: std::sync::Arc<UdpSocket>,
    addr: SocketAddr,
    codec: Codec,
    params: Option<Params>,
    pending: Vec<PendingOut>,
    exec: Option<(u32, oneshot::Sender<Result<Rows>>)>,
    cur_return_code: u32,
    pending_rows: Rows,
    voice_sink: Option<broadcast::Sender<VoiceData>>,
    last_packet: Instant,
    last_ping: Instant,
}

struct ActorShared {
    notify_tx: broadcast::Sender<Command>,
    voice_tx: broadcast::Sender<VoiceData>,
    closed_tx: tokio::sync::watch::Sender<bool>,
}

async fn run_actor(
    sock: UdpSocket,
    addr: SocketAddr,
    opts: HandshakeOptions,
    private_key: proto::EccKeyPrivP256,
    mut req_rx: mpsc::Receiver<Request>,
    notify_tx: broadcast::Sender<Command>,
    voice_tx: broadcast::Sender<VoiceData>,
    closed_tx: tokio::sync::watch::Sender<bool>,
    done_tx: oneshot::Sender<Result<HandshakeResult>>,
) {
    let sock = std::sync::Arc::new(sock);
    let (udp_tx, mut udp_rx) = mpsc::channel::<Vec<u8>>(512);
    {
        let sock = sock.clone();
        tokio::spawn(async move {
            let mut buf = vec![0u8; proto::MAX_UDP_PACKET_LENGTH + 64];
            loop {
                match sock.recv_from(&mut buf).await {
                    Ok((n, _)) => {
                        if udp_tx.send(buf[..n].to_vec()).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let shared = ActorShared {
        notify_tx,
        voice_tx,
        closed_tx,
    };
    let mut actor = Actor {
        sock,
        addr,
        codec: Codec::new(),
        params: None,
        pending: Vec::new(),
        exec: None,
        cur_return_code: 0,
        pending_rows: Vec::new(),
        voice_sink: None,
        last_packet: Instant::now(),
        last_ping: Instant::now(),
    };

    // ---- Phase 1: handshake ----
    let result = actor.handshake(&opts, &private_key, &mut udp_rx).await;
    let handshake_ok = result.is_ok();
    let _ = done_tx.send(result);
    if !handshake_ok {
        let _ = shared.closed_tx.send(true);
        return;
    }

    // ---- Phase 2: normal operation ----
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            biased;
            datagram = udp_rx.recv() => {
                match datagram {
                    Some(d) => actor.handle_udp(&shared, d),
                    None => break,
                }
            }
            req = req_rx.recv() => {
                match req {
                    Some(Request::Exec { cmd, reply }) => {
                        actor.exec(cmd, reply)
                    }
                    Some(Request::SendVoice { content, p_type }) => actor.send_voice(content, p_type),
                    Some(Request::SetVoiceSink { tx }) => actor.voice_sink = Some(tx),
                    Some(Request::Disconnect { reasonid, reasonmsg }) => {
                        let cmd = Command::new("clientdisconnect")
                            .param("reasonid", reasonid)
                            .param("reasonmsg", reasonmsg);
                        actor.send_command(cmd, false);
                    }
                    None => break,
                }
            }
            _ = tick.tick() => {
                if actor.maintenance().is_err() {
                    break;
                }
            }
        }
    }
    let _ = shared.closed_tx.send(true);
}

impl Actor {
    // ---- handshake ----

    async fn handshake(
        &mut self,
        opts: &HandshakeOptions,
        private_key: &proto::EccKeyPrivP256,
        udp_rx: &mut mpsc::Receiver<Vec<u8>>,
    ) -> Result<HandshakeResult> {
        let version = (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
            .saturating_sub(VERSION_TS_OFFSET)) as u32;

        let random0: [u8; 4] = rand::random();
        let alpha: [u8; 10] = rand::random();

        // Step 0 + 1 + 2 + 3 (with 127-restart loop).
        let init3 = 'outer: loop {
            let raw = proto::packet::build_c2s_init0(version, version, &random0);
            self.send_udp(raw);
            let pkt = self
                .wait_init(udp_rx, &[1])
                .await
                .map_err(|_| Error::Timeout)?;
            let (random1, random0_r) = match pkt {
                S2CInitData::Init1 { random1, random0_r } => (random1, random0_r),
                _ => return Err(Error::Protocol("expected init1".into())),
            };
            let raw = proto::packet::build_c2s_init2(version, &random1, &random0_r);
            self.send_udp(raw);
            let pkt = self
                .wait_init(udp_rx, &[3, 127])
                .await
                .map_err(|_| Error::Timeout)?;
            match pkt {
                S2CInitData::Init127 => continue 'outer,
                S2CInitData::Init3 { x, n, level, random2 } => {
                    break 'outer (x, n, level, random2);
                }
                _ => return Err(Error::Protocol("expected init3".into())),
            }
        };
        let (x, n, level, random2) = init3;
        if level > 10_000_000 {
            return Err(Error::Protocol("rsa puzzle level too high".into()));
        }

        // Solve the RSA puzzle: y = x^(2^level) mod n.
        let xi = num_bigint_dig::BigUint::from_bytes_be(&x);
        let ni = num_bigint_dig::BigUint::from_bytes_be(&n);
        let e = num_bigint_dig::BigUint::from(1u8) << level as usize;
        let yi = xi.modpow(&e, &ni);
        let mut y = [0u8; 64];
        let yb = yi.to_bytes_be();
        y[64 - yb.len()..].copy_from_slice(&yb);

        // clientinitiv command embedded in Init4.
        let omega = private_key.to_pub().to_tomcrypt();
        let raw = proto::packet::build_c2s_init4(version, &x, &n, level, &random2, &y, &alpha, &omega, "");
        self.send_udp(raw);

        // Await initivexpand2 (fake-encrypted command, server Command PId 0).
        let cmd = loop {
            let data = self
                .wait_datagram(udp_rx, |p_type, _| p_type == PacketType::Command)
                .await
                .ok_or(Error::Timeout)?;
            if let Some(c) = proto::decrypt_fake(&mut data.clone(), Direction::S2C).ok() {
                let cmd = Command::parse(&String::from_utf8_lossy(&c))?;
                // Ack the command and advance the receive window so the
                // following initserver fragments are considered in order.
                self.queue_ack(PacketType::Command, header_pid(&data));
                self.codec.advance_incoming(PacketType::Command, header_pid(&data));
                break cmd;
            }
        };

        if cmd.name != "initivexpand2" {
            return Err(Error::Protocol(format!("expected initivexpand2, got {}", cmd.name)));
        }
        let ot = cmd.get("ot").unwrap_or("");
        if ot != "1" {
            return Err(Error::Protocol("server is outdated (ot != 1)".into()));
        }
        let l_b64 = cmd.get("l").ok_or_else(|| Error::Protocol("initivexpand2 missing l".into()))?;
        let beta_b64 = cmd
            .get("beta")
            .ok_or_else(|| Error::Protocol("initivexpand2 missing beta".into()))?;
        let proof = cmd
            .get("proof")
            .ok_or_else(|| Error::Protocol("initivexpand2 missing proof".into()))?;
        let omega_str = cmd
            .get("omega")
            .ok_or_else(|| Error::Protocol("initivexpand2 missing omega".into()))?;
        let root_arg = cmd.get("root");

        use base64::Engine as _;
        let engine = base64::engine::general_purpose::STANDARD;
        let l = engine.decode(l_b64).map_err(|e| Error::Crypto(format!("bad license b64: {e}")))?;
        let beta_v = engine.decode(beta_b64).map_err(|e| Error::Crypto(format!("bad beta b64: {e}")))?;
        if beta_v.len() != 54 {
            return Err(Error::Crypto(format!("beta length {} != 54", beta_v.len())));
        }
        let mut beta = [0u8; 54];
        beta.copy_from_slice(&beta_v);
        let proof = engine.decode(proof).map_err(|e| Error::Crypto(format!("bad proof b64: {e}")))?;

        // Verify the license signature with the server's P-256 key.
        let server_key = proto::EccKeyPubP256::from_ts(omega_str)?;
        proto::EccKeyPrivP256::ecdsa_verify(&server_key, &l, &proof)?;

        // Derive the server ephemeral key from the license chain.
        let licenses = proto::Licenses::parse(l)?;
        let root = match root_arg {
            Some(r) => {
                let bytes = engine
                    .decode(r)
                    .map_err(|e| Error::Crypto(format!("bad root b64: {e}")))?;
                let mut rk = [0u8; 32];
                if bytes.len() != 32 {
                    return Err(Error::Crypto("root must be 32 bytes".into()));
                }
                rk.copy_from_slice(&bytes);
                curve25519_dalek::edwards::CompressedEdwardsY(rk)
                    .decompress()
                    .ok_or_else(|| Error::Crypto("invalid root point".into()))?
            }
            None => curve25519_dalek::edwards::CompressedEdwardsY(proto::ROOT_KEY)
                .decompress()
                .ok_or_else(|| Error::Crypto("invalid root point".into()))?,
        };
        let server_ek = licenses.derive_public_key_from(root)?;

        // Our ephemeral key + shared secrets.
        let mut ek_bytes = [0u8; 32];
        use rand::Rng;
        rand::thread_rng().fill(&mut ek_bytes);
        let ek_scalar = curve25519_dalek::scalar::Scalar::from_bytes_mod_order(ek_bytes);
        let (shared_iv, shared_mac) = proto::compute_iv_mac(&alpha, &beta, &ek_scalar, &server_ek);

        // Verify the server uid pin if configured.
        if let Some(pin) = &opts.server_uid_pin {
            let real = server_key.get_uid();
            if &real != pin {
                return Err(Error::Crypto(format!(
                    "server uid mismatch: expected {pin}, got {real}"
                )));
            }
        }

        let params = Params {
            c_id: 0,
            voice_encryption: true,
            shared_iv,
            shared_mac,
            key_cache: proto::new_key_cache(),
        };
        self.params = Some(params);

        // Send clientek: ek (compressed edwards point) + ECDSA(identity, ek||beta).
        let ek_pub = server_ek; // placeholder to avoid unused warnings below
        let _ = ek_pub;
        let ek_point = curve25519_dalek::constants::ED25519_BASEPOINT_POINT * ek_scalar;
        let ek_bytes = ek_point.compress().to_bytes();
        let mut signed = Vec::with_capacity(32 + 54);
        signed.extend_from_slice(&ek_bytes);
        signed.extend_from_slice(&beta);
        let ek_proof = private_key.sign(&signed);
        let clientek = Command::new("clientek")
            .param("ek", engine.encode(ek_bytes))
            .param("proof", engine.encode(ek_proof));
        // PId 1, fake-encrypted (encoded in send_command via force_fake).
        self.send_command(clientek, true);

        // Wait for the ack of clientek (Command PId 1) or an error.
        self.wait_ack_or_error(udp_rx, PacketType::Command, 1).await?;

        // Send clientinit (Command PId 2, real-encrypted).
        let mut clientinit = Command::new("clientinit")
            .param("client_nickname", &opts.nickname)
            .param("client_version", &opts.version)
            .param("client_platform", &opts.platform)
            .param("client_key_offset", opts.client_key_offset)
            .param("client_version_sign", &opts.version_sign);
        // Optional fields — bisected against 3.13.8 server behavior.
        if opts.input_muted {
            clientinit = clientinit.param("client_input_muted", 1);
        }
        if opts.output_muted {
            clientinit = clientinit.param("client_output_muted", 1);
        }
        if !opts.default_channel.is_empty() {
            clientinit = clientinit.param("client_default_channel", &opts.default_channel);
        }
        if !opts.channel_password.is_empty() {
            clientinit = clientinit.param("client_default_channel_password", &opts.channel_password);
        }
        if !opts.server_password.is_empty() {
            clientinit = clientinit.param("client_server_password", &opts.server_password);
        }
        clientinit = clientinit
            .param("client_nickname_phonetic", "")
            .param("client_meta_data", "")
            .param("client_default_token", "")
            .param("client_hardware_id", "");
        self.send_command(clientinit, false);

        // Wait for initserver (acks Command PId 2) or an error packet.
        let initserver = loop {
            let data = self
                .wait_datagram(udp_rx, |p_type, _| p_type == PacketType::Command)
                .await
                .ok_or(Error::Timeout)?;
            let p_id = header_pid(&data);
            self.queue_ack(PacketType::Command, p_id);
            let Some(content) = self.decrypt_packet(&data, PacketType::Command, p_id) else {
                continue;
            };
            let mut raw = data;
            let header_len = Direction::S2C.header_len();
            raw[header_len..].copy_from_slice(&content);
            let maybe = self.handle_command_packet(&raw, PacketType::Command, p_id)?;
            self.codec.advance_incoming(PacketType::Command, p_id);
            match maybe {
                Some(cmd) => {
                    match cmd.name.as_str() {
                        "initserver" => {
                            self.remove_pending(PacketType::Command, 2);
                            break cmd;
                        }
                        "error" => {
                            let id: i32 = cmd.get("id").and_then(|v| v.parse().ok()).unwrap_or(-1);
                            let msg = cmd.get("msg").unwrap_or("").to_string();
                            return Err(Error::Server { id, msg, extra: Vec::new() });
                        }
                        _ => { /* notification during handshake — ignore */ }
                    }
                }
                None => { /* fragment accumulated */ }
            }
        };

        let clid: u16 = initserver
            .get("aclid")
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| Error::Protocol("initserver missing aclid".into()))?;
        self.params.as_mut().unwrap().c_id = clid;

        Ok(HandshakeResult {
            params: self.params.clone().unwrap(),
            clid,
            server_key,
        })
    }

    async fn wait_init(
        &mut self,
        udp_rx: &mut mpsc::Receiver<Vec<u8>>,
        steps: &[u8],
    ) -> Result<S2CInitData> {
        let deadline = Instant::now() + HANDSHAKE_STEP_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let data = match tokio::time::timeout(remaining, udp_rx.recv()).await {
                Ok(Some(d)) => d,
                Ok(None) => return Err(Error::Closed),
                Err(_) => return Err(Error::Timeout),
            };
            if let Ok((_, s2c)) = proto::parse_s2c_init(&data) {
                let step = match &s2c {
                    S2CInitData::Init1 { .. } => 1,
                    S2CInitData::Init3 { .. } => 3,
                    S2CInitData::Init127 => 127,
                };
                if steps.contains(&step) {
                    return Ok(s2c);
                }
            }
            // Not the init packet we wanted: drop (or handle ping later).
        }
    }

    async fn wait_datagram(
        &mut self,
        udp_rx: &mut mpsc::Receiver<Vec<u8>>,
        filter: impl Fn(PacketType, &Header<'_>) -> bool,
    ) -> Option<Vec<u8>> {
        let deadline = Instant::now() + HANDSHAKE_STEP_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            let data = match tokio::time::timeout(remaining, udp_rx.recv()).await {
                Ok(Some(d)) => d,
                Ok(None) => return None,
                Err(_) => return None,
            };
            if let Ok(h) = Header::new(Direction::S2C, &data) {
                if let Ok(t) = h.packet_type() {
                    if filter(t, &h) {
                        return Some(data);
                    }
                    if t == PacketType::Ping {
                        self.send_pong(h.packet_id());
                    }
                }
            }
        }
    }

    /// Wait until the given outgoing packet is acked (an incoming Ack whose
    /// content equals `p_id` of `p_type`), or an error command arrives.
    async fn wait_ack_or_error(
        &mut self,
        udp_rx: &mut mpsc::Receiver<Vec<u8>>,
        p_type: PacketType,
        p_id: u16,
    ) -> Result<()> {
        let deadline = Instant::now() + HANDSHAKE_STEP_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::Timeout);
            }
            let data = match tokio::time::timeout(remaining, udp_rx.recv()).await {
                Ok(Some(d)) => d,
                Ok(None) => return Err(Error::Closed),
                Err(_) => return Err(Error::Timeout),
            };
            let Ok(header) = Header::new(Direction::S2C, &data) else {
                continue;
            };
            match header.packet_type() {
                Ok(PacketType::Ack) | Ok(PacketType::AckLow) => {
                    // Skip the ignorable server ack (header id 1).
                    if header.packet_id() == 1 {
                        continue;
                    }
                    let content = self.decrypt_or_fake(&data, header.packet_type().unwrap(), header.packet_id());
                    if let Some(c) = content {
                        if c.len() >= 2 {
                            let acked = u16::from_be_bytes([c[0], c[1]]);
                            if acked == p_id {
                                return Ok(());
                            }
                        }
                    }
                }
                Ok(PacketType::Command) => {
                    // Always ack received commands so the server stops
                    // resending them.
                    self.queue_ack(PacketType::Command, header.packet_id());
                    let content = self.decrypt_or_fake(&data, PacketType::Command, header.packet_id());
                    if let Some(c) = content {
                        let header_len = Direction::S2C.header_len();
                        let cmd = Command::parse(&String::from_utf8_lossy(&c[header_len..]))
                            .map_err(|e| Error::Protocol(format!("bad command: {e}")))?;
                        if cmd.name == "error" {
                            let id: i32 = cmd.get("id").and_then(|v| v.parse().ok()).unwrap_or(-1);
                            let msg = cmd.get("msg").unwrap_or("").to_string();
                            return Err(Error::Server { id, msg, extra: Vec::new() });
                        }
                    }
                }
                Ok(PacketType::Ping) => {
                    self.send_pong(header.packet_id());
                }
                _ => {}
            }
        }
    }

    fn send_pong(&mut self, ping_id: u16) {
        let pong_id = self.codec.next_out(PacketType::Pong);
        let raw = proto::build_packet(
            Direction::C2S,
            Flags::empty(),
            PacketType::Pong,
            pong_id,
            0,
            &ping_id.to_be_bytes(),
        );
        self.send_udp(raw);
    }

    // ---- normal operation ----

    fn exec(&mut self, cmd: Command, reply: oneshot::Sender<Result<Rows>>) {
        if self.exec.is_some() {
            let _ = reply.send(Err(Error::Other("concurrent client command".into())));
            return;
        }
        self.pending_rows.clear();
        // The server echoes `return_code` in the error packet that completes
        // this command.
        let code = self.cur_return_code;
        self.cur_return_code += 1;
        let cmd = cmd.param("return_code", code);
        self.exec = Some((code, reply));
        self.send_command(cmd, false);
    }

    fn send_command(&mut self, cmd: Command, force_fake: bool) {
        let content = cmd.serialize().into_bytes();
        let p_type = PacketType::Command;
        let base_id = self.codec.outgoing_p_ids[p_type as usize];
        let c_id = self.params.as_ref().map(|p| p.c_id).unwrap_or(0);
        let mut pieces = compress_and_split(&content);
        // Client commands always carry the NEWPROTOCOL flag.
        for piece in &mut pieces {
            piece.flags |= Flags::NEWPROTOCOL;
        }
        let mut datagrams = VecDeque::with_capacity(pieces.len());
        let mut last_id = base_id;
        for piece in pieces {
            let p_id = self.codec.next_out(p_type);
            last_id = p_id;
            let mut raw = proto::build_packet(
                Direction::C2S,
                piece.flags,
                p_type,
                p_id,
                c_id,
                &piece.data,
            );
            self.encrypt(&mut raw, p_type, p_id, force_fake && p_id == base_id);
            datagrams.push_back(raw);
        }
        self.pending.push(PendingOut {
            datagrams,
            p_type,
            p_id: last_id,
            last_sent: Instant::now(),
            resends: 0,
        });
        self.flush_pending();
    }

    fn send_voice(&mut self, content: Vec<u8>, p_type: PacketType) {
        let p_id = self.codec.next_out(p_type);
        let mut full = Vec::with_capacity(2 + content.len());
        full.extend_from_slice(&p_id.to_be_bytes());
        full.extend_from_slice(&content);
        let c_id = self.params.as_ref().map(|p| p.c_id).unwrap_or(0);
        let mut raw = proto::build_packet(Direction::C2S, Flags::empty(), p_type, p_id, c_id, &full);
        let voice_enc = self.params.as_ref().map(|p| p.voice_encryption).unwrap_or(false);
        if voice_enc {
            self.encrypt(&mut raw, p_type, p_id, false);
        } else {
            self.mark_unencrypted(&mut raw);
        }
        self.send_udp(raw);
    }

    fn encrypt(&mut self, raw: &mut Vec<u8>, p_type: PacketType, p_id: u16, force_fake: bool) {
        let generation = self.codec.out_generation(p_type);
        match &mut self.params {
            Some(params) if !force_fake => {
                let iv = params.shared_iv;
                let key_cache = &mut params.key_cache;
                let _ = proto::encrypt(
                    raw,
                    Direction::C2S,
                    p_type,
                    true,
                    p_id,
                    generation,
                    &iv,
                    key_cache,
                );
            }
            _ => {
                let _ = proto::encrypt_fake(raw, Direction::C2S);
            }
        }
    }

    fn mark_unencrypted(&mut self, raw: &mut Vec<u8>) {
        let pt_index = proto::C2S_HEADER_LEN - 1;
        raw[pt_index] |= Flags::UNENCRYPTED.bits();
        if let Some(params) = &self.params {
            let mac = params.shared_mac;
            raw[..8].copy_from_slice(&mac);
        }
    }

    fn send_udp(&mut self, raw: Vec<u8>) {
        let _ = self.sock.try_send_to(&raw, self.addr);
    }

    fn flush_pending(&mut self) {
        let datagrams: Vec<Vec<u8>> = self
            .pending
            .iter()
            .flat_map(|p| p.datagrams.iter().cloned())
            .collect();
        for d in datagrams {
            self.send_udp(d);
        }
        let now = Instant::now();
        for p in &mut self.pending {
            p.last_sent = now;
        }
    }

    fn maintenance(&mut self) -> Result<()> {
        let now = Instant::now();
        let mut to_send: Vec<Vec<u8>> = Vec::new();
        let mut give_up = false;
        for p in &mut self.pending {
            let elapsed = now.duration_since(p.last_sent);
            let delay = Duration::from_millis(350u64 << p.resends.min(6));
            if elapsed >= delay {
                p.resends += 1;
                to_send.extend(p.datagrams.iter().cloned());
                p.last_sent = now;
            }
            if p.resends >= 12 {
                give_up = true;
            }
        }
        self.pending.retain(|p| p.resends < 12);
        for d in to_send {
            self.send_udp(d);
        }
        if give_up {
            tracing::warn!("client packet never acknowledged");
            return Err(Error::Timeout);
        }

        if now.duration_since(self.last_packet) > Duration::from_secs(30) {
            tracing::warn!("client connection timed out (30s without packets)");
            return Err(Error::Timeout);
        }
        if self.params.is_some() && now.duration_since(self.last_ping) >= Duration::from_secs(1) {
            self.last_ping = now;
            let p_id = self.codec.next_out(PacketType::Ping);
            let raw = proto::build_packet(Direction::C2S, Flags::empty(), PacketType::Ping, p_id, 0, &[]);
            self.send_udp(raw);
        }
        Ok(())
    }

    fn handle_udp(&mut self, shared: &ActorShared, mut data: Vec<u8>) {
        self.last_packet = Instant::now();
        let Ok(header) = Header::new(Direction::S2C, &data) else {
            return;
        };
        let Ok(p_type) = header.packet_type() else {
            return;
        };
        let p_id = header.packet_id();

        match p_type {
            PacketType::Init => {}
            PacketType::Command | PacketType::CommandLow => {
                if !self.codec.in_receive_window(p_type, p_id) {
                    self.queue_ack(p_type, p_id);
                    return;
                }
                let Some(content) = self.decrypt_or_fake(&data, p_type, p_id) else {
                    return;
                };
                let header_len = Direction::S2C.header_len();
                data[header_len..].copy_from_slice(&content);

                match self.handle_command_packet(&data, p_type, p_id) {
                    Ok(maybe) => {
                        // Every in-order packet advances the receive window —
                        // fragments included.
                        self.codec.advance_incoming(p_type, p_id);
                        if let Some(cmd) = maybe {
                            if cmd.name == "initivexpand2" {
                                self.remove_pending(
                                    PacketType::Init,
                                    proto::packet::INIT_PACKET_ID,
                                );
                            } else if cmd.name == "initserver" {
                                self.remove_pending(PacketType::Command, 2);
                            }
                            self.queue_ack(p_type, p_id);
                            self.dispatch_command(shared, cmd);
                        }
                    }
                    Err(e) => tracing::warn!(error = %e, "command packet error"),
                }
            }
            PacketType::Ack | PacketType::AckLow => {
                if p_type == PacketType::Ack && p_id == 1 {
                    // Ignorable: `initserver` acks clientinit instead.
                    return;
                }
                if let Some(c) = self.decrypt_or_fake(&data, p_type, p_id) {
                    if c.len() >= 2 {
                        let acked = u16::from_be_bytes([c[0], c[1]]);
                        let for_type = if p_type == PacketType::Ack {
                            PacketType::Command
                        } else {
                            PacketType::CommandLow
                        };
                        self.remove_pending(for_type, acked);
                    }
                }
            }
            PacketType::Ping => {
                self.send_pong(p_id);
            }
            PacketType::Pong => {}
            PacketType::Voice | PacketType::VoiceWhisper => {
                let unencrypted = header
                    .flags()
                    .map(|f| f.contains(Flags::UNENCRYPTED))
                    .unwrap_or(false);
                let content = if unencrypted {
                    Some(header.content().to_vec())
                } else {
                    self.decrypt_packet(&data, p_type, p_id)
                };
                if let Some(c) = content {
                    if let Ok(v) = proto::parse_voice(Direction::S2C, Flags::empty(), &c) {
                        let _ = shared.voice_tx.send(v.clone());
                        if let Some(sink) = &self.voice_sink {
                            let _ = sink.send(v);
                        }
                    }
                }
            }
        }
    }

    fn decrypt_packet(&mut self, data: &[u8], p_type: PacketType, p_id: u16) -> Option<Vec<u8>> {
        let params = self.params.as_mut()?;
        let generation = if p_type.is_command() {
            self.codec.incoming_command_generation
        } else {
            0
        };
        let mut raw = data.to_vec();
        proto::decrypt(
            &mut raw,
            Direction::S2C,
            p_type,
            p_id,
            generation,
            &params.shared_iv,
            &mut params.key_cache,
        )
        .ok()
    }

    fn decrypt_or_fake(&mut self, data: &[u8], p_type: PacketType, p_id: u16) -> Option<Vec<u8>> {
        if let Some(c) = self.decrypt_packet(data, p_type, p_id) {
            return Some(c);
        }
        proto::decrypt_fake(&mut data.to_vec(), Direction::S2C).ok()
    }

    /// Defragment/reorder command packets (mirrors tsproto).
    fn handle_command_packet(
        &mut self,
        raw: &[u8],
        p_type: PacketType,
        p_id: u16,
    ) -> Result<Option<Command>> {
        let cmd_i = if p_type == PacketType::Command { 0 } else { 1 };
        let cur_next = self.codec.incoming_p_ids[p_type as usize];
        if cur_next != p_id {
            tracing::debug!(got = p_id, expected = cur_next, "out of order command");
            self.codec.receive_queue[cmd_i].push_back(raw.to_vec());
            return Ok(None);
        }

        let header_len = Direction::S2C.header_len();
        let flags = Header::new(Direction::S2C, raw)?.flags()?;
        let mut complete: Option<Vec<u8>> = None;
        if flags.contains(Flags::FRAGMENTED) {
            if let Some(mut frag) = self.codec.fragmented_queue[cmd_i].take() {
                frag.extend_from_slice(&raw[header_len..]);
                complete = Some(frag);
            } else {
                // First fragment.
                self.codec.fragmented_queue[cmd_i] = Some(raw.to_vec());
                return Ok(None);
            }
        } else if let Some(frag) = &mut self.codec.fragmented_queue[cmd_i] {
            // Middle fragment.
            frag.extend_from_slice(&raw[header_len..]);
            if frag.len() > proto::MAX_FRAGMENTS_LENGTH {
                self.codec.fragmented_queue[cmd_i] = None;
                return Err(Error::Protocol("fragment queue overflow".into()));
            }
            return Ok(None);
        } else {
            complete = Some(raw.to_vec());
        }

        let data = complete.unwrap();
        let flags = Header::new(Direction::S2C, &data)?.flags()?;
        let payload = if flags.contains(Flags::COMPRESSED) {
            let content = &data[header_len..];
            let decompressed = quicklz::decompress(&mut &content[..], 2 * 1024 * 1024)
                .map_err(|e| Error::Protocol(format!("quicklz: {e}")))?;
            decompressed
        } else {
            data[header_len..].to_vec()
        };
        Command::parse(&String::from_utf8_lossy(&payload)).map(Some)
    }

    fn queue_ack(&mut self, p_type: PacketType, p_id: u16) {
        let ack_type = if p_type == PacketType::Command {
            PacketType::Ack
        } else {
            PacketType::AckLow
        };
        let ack_id = self.codec.next_out(ack_type);
        let c_id = self.params.as_ref().map(|p| p.c_id).unwrap_or(0);
        let mut raw = proto::build_packet(
            Direction::C2S,
            Flags::empty(),
            ack_type,
            ack_id,
            c_id,
            &p_id.to_be_bytes(),
        );
        let force_fake = ack_type == PacketType::Ack && ack_id == 0;
        self.encrypt(&mut raw, ack_type, ack_id, force_fake);
        self.send_udp(raw);
    }

    fn remove_pending(&mut self, p_type: PacketType, p_id: u16) {
        self.pending
            .retain(|p| !(p.p_type == p_type && p.p_id == p_id));
    }

    fn dispatch_command(&mut self, shared: &ActorShared, cmd: Command) {
        // `error` completes the pending request; `notify*` are notifications;
        // everything else while a request is pending is its data rows (the
        // response to `whoami` has no command name at all). Server pushes
        // without a pending request (login dumps) are broadcast.
        if cmd.name == "error" {
            // Match the completion to the pending request via the echoed
            // return_code (falling back to the single pending request).
            let echo = cmd.get("return_code").and_then(|v| v.parse::<u32>().ok());
            let matches = match (&self.exec, echo) {
                (Some((code, _)), Some(e)) => e == *code,
                (Some(_), None) => true,
                (None, _) => false,
            };
            if matches {
                if let Some((_, reply)) = self.exec.take() {
                    let id: i32 = cmd.get("id").and_then(|v| v.parse().ok()).unwrap_or(-1);
                    if id == 0 {
                        let rows = std::mem::take(&mut self.pending_rows);
                        let _ = reply.send(Ok(rows));
                    } else {
                        let msg = cmd.get("msg").unwrap_or("").to_string();
                        let extra = cmd
                            .params
                            .first()
                            .cloned()
                            .unwrap_or_default()
                            .into_iter()
                            .filter(|(k, _)| k != "id" && k != "msg")
                            .collect();
                        self.pending_rows.clear();
                        let _ = reply.send(Err(Error::Server { id, msg, extra }));
                    }
                }
                return;
            }
            // An error for an already-completed request: ignore.
            return;
        }
        let is_pending = self.exec.is_some();
        if cmd.name.starts_with("notify") || !is_pending {
            let _ = shared.notify_tx.send(cmd);
        } else {
            self.pending_rows.extend(cmd.params);
        }
    }
}

fn header_pid(data: &[u8]) -> u16 {
    u16::from_be_bytes([data[8], data[9]])
}

struct Piece {
    flags: Flags,
    data: Vec<u8>,
}

fn compress_and_split(content: &[u8]) -> Vec<Piece> {
    let max = proto::MAX_COMMAND_DATA;
    if content.len() <= max {
        return vec![Piece {
            flags: Flags::empty(),
            data: content.to_vec(),
        }];
    }
    let compressed = quicklz::compress(content, quicklz::CompressionLevel::Lvl1);
    let (data, compressed_flag) = if compressed.len() < content.len() {
        (compressed, true)
    } else {
        (content.to_vec(), false)
    };
    let count = data.len().div_ceil(max);
    let mut out = Vec::with_capacity(count);
    for (i, chunk) in data.chunks(max).enumerate() {
        let mut f = Flags::empty();
        if compressed_flag && i == 0 {
            f |= Flags::COMPRESSED;
        }
        if i == 0 || i == count - 1 {
            f |= Flags::FRAGMENTED;
        }
        out.push(Piece {
            flags: f,
            data: chunk.to_vec(),
        });
    }
    out
}
