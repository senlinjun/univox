//! UDP packet header, Init1 handshake steps, voice and ack packets
//! (see ReSpeak/tsdeclarations `ts3protocol.md`).
//!
//! C→S: `MAC(8) | PId(u16) | CId(u16) | PT(u8) | data`
//! S→C: `MAC(8) | PId(u16) | PT(u8) | data`

use bitflags::bitflags;

use crate::error::{Error, Result};

pub const MAC_LEN: usize = 8;
pub const S2C_HEADER_LEN: usize = 11;
pub const C2S_HEADER_LEN: usize = 13;
/// Max UDP payload including header.
pub const MAX_UDP_PACKET_LENGTH: usize = 500;
/// Packet id used for all Init1 packets (0x65 = 101).
pub const INIT_PACKET_ID: u16 = 0x65;
pub const INIT1_MAC: &[u8; 8] = b"TS3INIT1";
/// Max size of a command packet's payload (data part).
pub const MAX_COMMAND_DATA: usize = MAX_UDP_PACKET_LENGTH - C2S_HEADER_LEN;
/// Upper bound for accumulated fragmented command data.
pub const MAX_FRAGMENTS_LENGTH: usize = 0x4000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(usize)]
pub enum PacketType {
    Voice = 0,
    VoiceWhisper = 1,
    Command = 2,
    CommandLow = 3,
    Ping = 4,
    Pong = 5,
    Ack = 6,
    AckLow = 7,
    Init = 8,
}

pub const PACKET_TYPE_COUNT: usize = 9;

impl PacketType {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v & 0x0F {
            0 => PacketType::Voice,
            1 => PacketType::VoiceWhisper,
            2 => PacketType::Command,
            3 => PacketType::CommandLow,
            4 => PacketType::Ping,
            5 => PacketType::Pong,
            6 => PacketType::Ack,
            7 => PacketType::AckLow,
            8 => PacketType::Init,
            _ => return None,
        })
    }

    pub fn to_u8(self) -> u8 {
        self as u8
    }

    pub fn is_command(self) -> bool {
        matches!(self, PacketType::Command | PacketType::CommandLow)
    }

    pub fn is_voice(self) -> bool {
        matches!(self, PacketType::Voice | PacketType::VoiceWhisper)
    }

    pub fn is_ack(self) -> bool {
        matches!(self, PacketType::Ack | PacketType::AckLow | PacketType::Pong)
    }
}

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct Flags: u8 {
        const UNENCRYPTED = 0x80;
        const COMPRESSED  = 0x40;
        const NEWPROTOCOL = 0x20;
        const FRAGMENTED  = 0x10;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    S2C,
    C2S,
}

impl Direction {
    pub fn reverse(self) -> Self {
        match self {
            Direction::S2C => Direction::C2S,
            Direction::C2S => Direction::S2C,
        }
    }

    pub fn header_len(self) -> usize {
        match self {
            Direction::S2C => S2C_HEADER_LEN,
            Direction::C2S => C2S_HEADER_LEN,
        }
    }
}

/// A parsed packet header. Works for both directions; `client_id` is only
/// present for C2S packets.
#[derive(Debug, Clone, Copy)]
pub struct Header<'a> {
    pub direction: Direction,
    pub data: &'a [u8],
}

impl<'a> Header<'a> {
    pub fn new(direction: Direction, data: &'a [u8]) -> Result<Self> {
        if data.len() < direction.header_len() {
            return Err(Error::Protocol("packet shorter than header".into()));
        }
        Ok(Self { direction, data })
    }

    pub fn mac(&self) -> &[u8] {
        &self.data[..MAC_LEN]
    }

    pub fn packet_id(&self) -> u16 {
        let off = MAC_LEN;
        u16::from_be_bytes([self.data[off], self.data[off + 1]])
    }

    pub fn client_id(&self) -> Option<u16> {
        match self.direction {
            Direction::C2S => {
                let off = MAC_LEN + 2;
                Some(u16::from_be_bytes([self.data[off], self.data[off + 1]]))
            }
            Direction::S2C => None,
        }
    }

    pub fn packet_type(&self) -> Result<PacketType> {
        PacketType::from_u8(self.data[self.direction.header_len() - 1])
            .ok_or_else(|| Error::Protocol(format!("invalid packet type byte")))
    }

    pub fn flags(&self) -> Result<Flags> {
        let b = self.data[self.direction.header_len() - 1];
        Flags::from_bits(b & 0xF0).ok_or_else(|| Error::Protocol("invalid flags".into()))
    }

    pub fn content(&self) -> &'a [u8] {
        &self.data[self.direction.header_len()..]
    }

    /// The EAX associated data: the header minus the MAC (PId, CId, PT).
    pub fn meta(&self) -> &'a [u8] {
        &self.data[MAC_LEN..self.direction.header_len()]
    }
}

/// Build a raw packet: `header || content`, MAC zeroed (caller fills it).
pub fn build_packet(
    direction: Direction,
    flags: Flags,
    p_type: PacketType,
    packet_id: u16,
    client_id: u16,
    content: &[u8],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(direction.header_len() + content.len());
    out.extend_from_slice(&[0u8; MAC_LEN]);
    out.extend_from_slice(&packet_id.to_be_bytes());
    if direction == Direction::C2S {
        out.extend_from_slice(&client_id.to_be_bytes());
    }
    out.push(flags.bits() | p_type.to_u8());
    out.extend_from_slice(content);
    out
}

// ---- Init1 handshake ----

/// C→S Init1 step 0.
pub fn build_c2s_init0(version: u32, timestamp: u32, random0: &[u8; 4]) -> Vec<u8> {
    let mut content = Vec::with_capacity(13);
    content.extend_from_slice(&version.to_be_bytes());
    content.push(0);
    content.extend_from_slice(&timestamp.to_be_bytes());
    content.extend_from_slice(random0);
    content.extend_from_slice(&[0u8; 8]); // reserved
    let mut p = build_packet(Direction::C2S, Flags::empty(), PacketType::Init, INIT_PACKET_ID, 0, &[]);
    p[0..8].copy_from_slice(INIT1_MAC);
    p.extend_from_slice(&content);
    p
}

/// C→S Init1 step 2.
pub fn build_c2s_init2(version: u32, random1: &[u8; 16], random0_r: &[u8; 4]) -> Vec<u8> {
    let mut content = Vec::with_capacity(21);
    content.extend_from_slice(&version.to_be_bytes());
    content.push(2);
    content.extend_from_slice(random1);
    content.extend_from_slice(random0_r);
    let mut p = build_packet(Direction::C2S, Flags::empty(), PacketType::Init, INIT_PACKET_ID, 0, &[]);
    p[0..8].copy_from_slice(INIT1_MAC);
    p.extend_from_slice(&content);
    p
}

/// C→S Init1 step 4 (with the trailing `clientinitiv` command).
#[allow(clippy::too_many_arguments)]
pub fn build_c2s_init4(
    version: u32,
    x: &[u8; 64],
    n: &[u8; 64],
    level: u32,
    random2: &[u8; 100],
    y: &[u8; 64],
    alpha: &[u8; 10],
    omega: &[u8],
    ip: &str,
) -> Vec<u8> {
    use base64::Engine as _;
    let mut content = Vec::with_capacity(233);
    content.extend_from_slice(&version.to_be_bytes());
    content.push(4);
    content.extend_from_slice(x);
    content.extend_from_slice(n);
    content.extend_from_slice(&level.to_be_bytes());
    content.extend_from_slice(random2);
    content.extend_from_slice(y);
    let ip_part = if ip.is_empty() {
        String::new()
    } else {
        format!("={ip}")
    };
    content.extend_from_slice(
        format!(
            "clientinitiv alpha={} omega={} ot=1 ip{}",
            base64::engine::general_purpose::STANDARD.encode(alpha),
            base64::engine::general_purpose::STANDARD.encode(omega),
            ip_part
        )
        .as_bytes(),
    );
    let mut p = build_packet(Direction::C2S, Flags::empty(), PacketType::Init, INIT_PACKET_ID, 0, &[]);
    p[0..8].copy_from_slice(INIT1_MAC);
    p.extend_from_slice(&content);
    p
}

/// A parsed S→C Init1 packet.
#[derive(Debug, Clone, PartialEq)]
pub enum S2CInitData {
    Init1 { random1: [u8; 16], random0_r: [u8; 4] },
    Init3 { x: [u8; 64], n: [u8; 64], level: u32, random2: [u8; 100] },
    Init127,
}

pub fn parse_s2c_init(data: &[u8]) -> Result<(Header<'_>, S2CInitData)> {
    let header = Header::new(Direction::S2C, data)?;
    if header.packet_type()? != PacketType::Init || header.mac() != INIT1_MAC {
        return Err(Error::Protocol("not an init1 packet".into()));
    }
    let content = header.content();
    if content.is_empty() {
        return Err(Error::Protocol("empty init packet".into()));
    }
    let data = match content[0] {
        1 => {
            if content.len() < 21 {
                return Err(Error::Protocol("init1 packet too short".into()));
            }
            S2CInitData::Init1 {
                random1: content[1..17].try_into().unwrap(),
                random0_r: content[17..21].try_into().unwrap(),
            }
        }
        3 => {
            if content.len() < 233 {
                return Err(Error::Protocol("init3 packet too short".into()));
            }
            S2CInitData::Init3 {
                x: content[1..65].try_into().unwrap(),
                n: content[65..129].try_into().unwrap(),
                level: u32::from_be_bytes(content[129..133].try_into().unwrap()),
                random2: content[133..233].try_into().unwrap(),
            }
        }
        127 => S2CInitData::Init127,
        other => return Err(Error::Protocol(format!("invalid init step {other}"))),
    };
    Ok((header, data))
}

/// Parse the clientinitiv command embedded in an S2C init? No — the client
/// sends it; the server's init4 equivalent is the `initivexpand2` command.
/// (Kept for the dump tool.)
pub fn parse_c2s_init(data: &[u8]) -> Result<(Header<'_>, u32, u8)> {
    let header = Header::new(Direction::C2S, data)?;
    if header.packet_type()? != PacketType::Init || header.mac() != INIT1_MAC {
        return Err(Error::Protocol("not an init1 packet".into()));
    }
    let content = header.content();
    if content.len() < 5 {
        return Err(Error::Protocol("c2s init packet too short".into()));
    }
    let version = u32::from_be_bytes(content[0..4].try_into().unwrap());
    Ok((header, version, content[4]))
}

// ---- Voice ----

/// TS3 voice codec identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecType {
    SpeexNarrowband = 0,
    SpeexWideband = 1,
    SpeexUltrawideband = 2,
    CeltMono = 3,
    OpusVoice = 4,
    OpusMusic = 5,
}

/// The codec byte prefixed to Opus voice/whisper payloads. Lives here so
/// drivers without the `voice` feature (no libopus) can still build and
/// parse voice packets.
pub const CODEC_OPUS_VOICE: u8 = CodecType::OpusVoice as u8;

/// Parsed voice packet content.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceData {
    C2S { id: u16, codec: u8, data: Vec<u8> },
    C2SWhisper { id: u16, codec: u8, channels: Vec<u64>, clients: Vec<u16>, data: Vec<u8> },
    C2SWhisperNew { id: u16, codec: u8, whisper_type: u8, target: u8, target_id: u64, data: Vec<u8> },
    S2C { id: u16, from: u16, codec: u8, data: Vec<u8> },
    S2CWhisper { id: u16, from: u16, codec: u8, data: Vec<u8> },
}

pub fn build_c2s_voice(id: u16, codec: u8, data: &[u8]) -> Vec<u8> {
    let mut content = Vec::with_capacity(3 + data.len());
    content.extend_from_slice(&id.to_be_bytes());
    content.push(codec);
    content.extend_from_slice(data);
    content
}

pub fn build_c2s_whisper(
    id: u16,
    codec: u8,
    channels: &[u64],
    clients: &[u16],
    data: &[u8],
) -> Vec<u8> {
    let mut content = Vec::with_capacity(5 + 8 * channels.len() + 2 * clients.len() + data.len());
    content.extend_from_slice(&id.to_be_bytes());
    content.push(codec);
    content.push(channels.len() as u8);
    content.push(clients.len() as u8);
    for c in channels {
        content.extend_from_slice(&c.to_be_bytes());
    }
    for c in clients {
        content.extend_from_slice(&c.to_be_bytes());
    }
    content.extend_from_slice(data);
    content
}

pub fn parse_voice(direction: Direction, flags: Flags, content: &[u8]) -> Result<VoiceData> {
    if content.len() < 4 {
        return Err(Error::Protocol("voice packet too short".into()));
    }
    let id = u16::from_be_bytes([content[0], content[1]]);
    match direction {
        Direction::C2S => {
            let codec = content[2];
            let rest = &content[3..];
            if flags.contains(Flags::NEWPROTOCOL) {
                if rest.len() < 10 {
                    return Err(Error::Protocol("whisper-new packet too short".into()));
                }
                Ok(VoiceData::C2SWhisperNew {
                    id,
                    codec,
                    whisper_type: rest[0],
                    target: rest[1],
                    target_id: u64::from_be_bytes(rest[2..10].try_into().unwrap()),
                    data: rest[10..].to_vec(),
                })
            } else {
                // Plain voice; the legacy target-list whisper format is only
                // produced by [`build_c2s_whisper`].
                Ok(VoiceData::C2S {
                    id,
                    codec,
                    data: rest.to_vec(),
                })
            }
        }
        Direction::S2C => {
            let from = u16::from_be_bytes([content[2], content[3]]);
            let codec = content[4];
            let data = content[5..].to_vec();
            if flags.contains(Flags::NEWPROTOCOL) {
                Ok(VoiceData::S2CWhisper { id, from, codec, data })
            } else {
                Ok(VoiceData::S2C { id, from, codec, data })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init0_wire_format() {
        let p = build_c2s_init0(1461588969, 1_700_000_000, &[1, 2, 3, 4]);
        assert_eq!(p.len(), C2S_HEADER_LEN + 21);
        assert_eq!(&p[..8], b"TS3INIT1");
        assert_eq!(&p[8..10], &101u16.to_be_bytes());
        assert_eq!(p[12], PacketType::Init.to_u8());
        // version, step 0, timestamp, random, reserved
        assert_eq!(&p[C2S_HEADER_LEN..C2S_HEADER_LEN + 4], &1461588969u32.to_be_bytes());
        assert_eq!(p[C2S_HEADER_LEN + 4], 0);
    }

    #[test]
    fn init3_parse() {
        // Build a fake S2C init3 packet.
        let mut content = vec![3u8];
        content.extend_from_slice(&[1u8; 64]);
        content.extend_from_slice(&[2u8; 64]);
        content.extend_from_slice(&42u32.to_be_bytes());
        content.extend_from_slice(&[7u8; 100]);
        let mut p = b"TS3INIT1".to_vec();
        p.extend_from_slice(&101u16.to_be_bytes());
        p.push(PacketType::Init.to_u8());
        p.extend_from_slice(&content);

        let (h, data) = parse_s2c_init(&p).unwrap();
        assert_eq!(h.packet_id(), 101);
        match data {
            S2CInitData::Init3 { x, n, level, random2 } => {
                assert_eq!(x, [1u8; 64]);
                assert_eq!(n, [2u8; 64]);
                assert_eq!(level, 42);
                assert_eq!(random2, [7u8; 100]);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn voice_roundtrip() {
        let content = build_c2s_voice(5, CodecType::OpusVoice as u8, &[1, 2, 3]);
        let v = parse_voice(Direction::C2S, Flags::empty(), &content).unwrap();
        assert_eq!(
            v,
            VoiceData::C2S {
                id: 5,
                codec: 4,
                data: vec![1, 2, 3]
            }
        );

        // S2C with from id.
        let mut s2c = 9u16.to_be_bytes().to_vec();
        s2c.extend_from_slice(&12u16.to_be_bytes());
        s2c.push(CodecType::OpusMusic as u8);
        s2c.extend_from_slice(&[9, 9]);
        let v = parse_voice(Direction::S2C, Flags::empty(), &s2c).unwrap();
        assert_eq!(
            v,
            VoiceData::S2C {
                id: 9,
                from: 12,
                codec: 5,
                data: vec![9, 9]
            }
        );
    }

    #[test]
    fn header_meta() {
        let raw = build_packet(Direction::C2S, Flags::NEWPROTOCOL, PacketType::Command, 7, 3, b"x");
        let h = Header::new(Direction::C2S, &raw).unwrap();
        assert_eq!(h.packet_id(), 7);
        assert_eq!(h.client_id(), Some(3));
        assert_eq!(h.packet_type().unwrap(), PacketType::Command);
        assert!(h.flags().unwrap().contains(Flags::NEWPROTOCOL));
        assert_eq!(h.meta(), &raw[8..13]);
        assert_eq!(h.content(), b"x");
    }
}
