//! Per-packet EAX cryptography and key derivation (see tsproto `algorithms.rs`).
//!
//! - key/nonce = sha256(direction ‖ type ‖ generation ‖ SharedIV), with the
//!   key's first two bytes XORed by the packet id
//! - cipher: EAX over AES-128 with an 8-byte tag (the packet MAC)
//! - "fake" encryption uses a fixed key/nonce for handshake packets

use eax::aead::consts::U8;
use eax::{AeadInPlace, Eax, KeyInit};
use sha2::{Digest, Sha256, Sha512};

use crate::error::{Error, Result};
use crate::packet::{Direction, Flags, Header, PacketType, PACKET_TYPE_COUNT};

pub const FAKE_KEY: [u8; 16] = *b"c:\\windows\\syste";
pub const FAKE_NONCE: [u8; 16] = *b"m\\firewall32.cpl";

/// TeamSpeak license-system root key (Curve25519 Edwards point, compressed).
pub const ROOT_KEY: [u8; 32] = [
    0xcd, 0x0d, 0xe2, 0xae, 0xd4, 0x63, 0x45, 0x50, 0x9a, 0x7e, 0x3c, 0xfd, 0x8f, 0x68, 0xb3, 0xdc,
    0x75, 0x55, 0xb2, 0x9d, 0xcc, 0xec, 0x73, 0xcd, 0x18, 0x75, 0x0f, 0x99, 0x38, 0x12, 0x40, 0x8a,
];

/// Must always be encrypted once the crypto parameters are available.
pub fn must_encrypt(t: PacketType) -> bool {
    matches!(t, PacketType::Command | PacketType::CommandLow)
}

/// May be encrypted (ack packets and optionally voice).
pub fn should_encrypt(t: PacketType, voice_encryption: bool) -> bool {
    must_encrypt(t) || matches!(t, PacketType::Ack | PacketType::AckLow)
        || (voice_encryption && t.is_voice())
}

/// Cached key/nonce per (packet type, direction) for a generation.
#[derive(Clone, Copy)]
pub struct CachedKey {
    pub generation_id: u32,
    pub key: [u8; 16],
    pub nonce: [u8; 16],
}

impl Default for CachedKey {
    fn default() -> Self {
        Self {
            generation_id: u32::MAX,
            key: [0; 16],
            nonce: [0; 16],
        }
    }
}

/// Key cache: `[packet_type][has_client_id]`.
pub type KeyCache = [[CachedKey; 2]; PACKET_TYPE_COUNT];

pub fn new_key_cache() -> KeyCache {
    Default::default()
}

/// Direction byte: 0x31 when the packet carries a client id (C2S), else 0x30.
fn direction_byte(c_id: Option<u16>) -> u8 {
    if c_id.is_some() {
        0x31
    } else {
        0x30
    }
}

/// Derive the per-generation key and nonce for a packet type.
fn create_key_nonce(
    p_type: PacketType,
    c_id: Option<u16>,
    p_id: u16,
    generation_id: u32,
    iv: &[u8; 64],
    cache: &mut KeyCache,
) -> ([u8; 16], [u8; 16]) {
    let entry = &mut cache[p_type as usize][usize::from(c_id.is_some())];
    if entry.generation_id != generation_id {
        let mut temp = [0u8; 70];
        temp[0] = direction_byte(c_id);
        temp[1] = p_type.to_u8();
        temp[2..6].copy_from_slice(&generation_id.to_be_bytes());
        temp[6..].copy_from_slice(iv);
        let keynonce = Sha256::digest(temp);
        entry.generation_id = generation_id;
        entry.key.copy_from_slice(&keynonce[..16]);
        entry.nonce.copy_from_slice(&keynonce[16..]);
    }
    let mut key = entry.key;
    let nonce = entry.nonce;
    key[0] ^= (p_id >> 8) as u8;
    key[1] ^= (p_id & 0xff) as u8;
    (key, nonce)
}

fn eax_encrypt(key: &[u8; 16], nonce: &[u8; 16], meta: &[u8], content: &mut [u8]) -> Result<[u8; 8]> {
    let cipher = Eax::<aes::Aes128, U8>::new(key.into());
    let mac = cipher
        .encrypt_in_place_detached(nonce.into(), meta, content)
        .map_err(|_| Error::Crypto("EAX encryption failed".into()))?;
    let mut out = [0u8; 8];
    out.copy_from_slice(&mac);
    Ok(out)
}

fn eax_decrypt(key: &[u8; 16], nonce: &[u8; 16], meta: &[u8], content: &mut [u8], mac: &[u8]) -> Result<()> {
    let cipher = Eax::<aes::Aes128, U8>::new(key.into());
    cipher
        .decrypt_in_place_detached(nonce.into(), meta, content, mac.into())
        .map_err(|_| Error::Crypto("EAX MAC mismatch".into()))
}

/// Encrypt a full raw packet in place: computes the tag over the content and
/// writes it into the MAC field.
pub fn encrypt(
    raw: &mut Vec<u8>,
    direction: Direction,
    p_type: PacketType,
    has_client_id: bool,
    p_id: u16,
    generation_id: u32,
    iv: &[u8; 64],
    cache: &mut KeyCache,
) -> Result<()> {
    let header = Header::new(direction, raw)?;
    let (key, nonce) = create_key_nonce(p_type, has_client_id.then_some(0u16), p_id, generation_id, iv, cache);
    let meta = header.meta().to_vec();
    let header_len = direction.header_len();
    let mac = eax_encrypt(&key, &nonce, &meta, &mut raw[header_len..])?;
    raw[..8].copy_from_slice(&mac);
    Ok(())
}

/// Fake-encrypt a full raw packet in place (fixed key/nonce).
pub fn encrypt_fake(raw: &mut Vec<u8>, direction: Direction) -> Result<()> {
    let header = Header::new(direction, raw)?;
    let meta = header.meta().to_vec();
    let header_len = direction.header_len();
    let mac = eax_encrypt(&FAKE_KEY, &FAKE_NONCE, &meta, &mut raw[header_len..])?;
    raw[..8].copy_from_slice(&mac);
    Ok(())
}

/// Decrypt a full raw packet in place. Returns the decrypted content.
pub fn decrypt(
    raw: &mut [u8],
    direction: Direction,
    p_type: PacketType,
    p_id: u16,
    generation_id: u32,
    iv: &[u8; 64],
    cache: &mut KeyCache,
) -> Result<Vec<u8>> {
    let header = Header::new(direction, raw)?;
    let has_client_id = header.client_id().is_some();
    let (key, nonce) = create_key_nonce(p_type, has_client_id.then_some(0u16), p_id, generation_id, iv, cache);
    let meta = header.meta().to_vec();
    let mac = header.mac().to_vec();
    let header_len = direction.header_len();
    let mut content = raw[header_len..].to_vec();
    eax_decrypt(&key, &nonce, &meta, &mut content, &mac)?;
    Ok(content)
}

/// Fake-decrypt a full raw packet in place.
pub fn decrypt_fake(raw: &mut [u8], direction: Direction) -> Result<Vec<u8>> {
    let header = Header::new(direction, raw)?;
    let meta = header.meta().to_vec();
    let mac = header.mac().to_vec();
    let header_len = direction.header_len();
    let mut content = raw[header_len..].to_vec();
    eax_decrypt(&FAKE_KEY, &FAKE_NONCE, &meta, &mut content, &mac)?;
    Ok(content)
}

/// Compute the shared IV (64 bytes) and MAC (8 bytes) for the new protocol:
/// shared = x25519(ek_priv, server_ek_point); iv = sha512(shared) ⊕ alpha ⊕ beta.
pub fn compute_iv_mac(
    alpha: &[u8; 10],
    beta: &[u8; 54],
    ek_priv: &curve25519_dalek::scalar::Scalar,
    server_ek: &curve25519_dalek::edwards::EdwardsPoint,
) -> ([u8; 64], [u8; 8]) {
    let shared = (server_ek * ek_priv).compress().to_bytes();
    let mut shared_iv = [0u8; 64];
    shared_iv.copy_from_slice(&Sha512::digest(shared));
    for (i, a) in alpha.iter().enumerate() {
        shared_iv[i] ^= a;
    }
    for (i, b) in beta.iter().enumerate() {
        shared_iv[i + 10] ^= b;
    }
    let mac = sha1::Sha1::digest(shared_iv);
    let mut shared_mac = [0u8; 8];
    shared_mac.copy_from_slice(&mac[..8]);
    (shared_iv, shared_mac)
}

/// Number of leading zero bits of sha1(omega || offset) — the hash-cash level.
pub fn get_hash_cash_level(omega: &str, offset: u64) -> u8 {
    let data = sha1::Sha1::digest(format!("{omega}{offset}").as_bytes());
    let mut res = 0u8;
    for &d in data.iter() {
        if d == 0 {
            res += 8;
        } else {
            res += d.trailing_zeros() as u8;
            break;
        }
    }
    res
}

/// Solve the hash-cash puzzle for a given level.
pub fn hash_cash(omega: &str, level: u8) -> u64 {
    let mut offset = 0u64;
    while offset < u64::MAX && get_hash_cash_level(omega, offset) < level {
        offset += 1;
    }
    offset
}

/// Is this raw packet marked unencrypted?
pub fn is_unencrypted(raw: &[u8], direction: Direction) -> Result<bool> {
    let header = Header::new(direction, raw)?;
    Ok(header.flags()?.contains(Flags::UNENCRYPTED))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::build_packet;

    /// Reference vector from tsproto's `test_fake_encrypt`.
    #[test]
    fn fake_encrypt_reference() {
        // OutAck for Command id 0, C2S: content = 0u16, header PId 0.
        let mut raw = build_packet(Direction::C2S, Flags::empty(), PacketType::Ack, 0, 0, &[0, 0]);
        encrypt_fake(&mut raw, Direction::C2S).unwrap();
        let real_res: &[u8] = &[
            0xa4, 0x7b, 0x47, 0x94, 0xdb, 0xa9, 0x6a, 0xc5, 0, 0, 0, 0, 0x6, 0xfe, 0x18,
        ];
        assert_eq!(real_res, &raw[..]);
    }

    #[test]
    fn fake_roundtrip() {
        let mut raw = build_packet(Direction::C2S, Flags::empty(), PacketType::Ack, 3, 0, &[0, 5]);
        encrypt_fake(&mut raw, Direction::C2S).unwrap();
        let content = decrypt_fake(&mut raw, Direction::C2S).unwrap();
        assert_eq!(content, vec![0, 5]);
    }

    #[test]
    fn key_nonce_reference() {
        // From tsproto `shared_iv31` test: sha256(0x31,0x02,gen=0,iv) with
        // the known shared iv.
        let shared_iv: [u8; 64] = [
            0x7e, 0x34, 0xc4, 0xdf, 0x0a, 0x5d, 0xbb, 0xac, 0xc9, 0x2f, 0xd1, 0xa7, 0xd2, 0x48,
            0x6c, 0x2e, 0xa2, 0xf4, 0x17, 0x97, 0x85, 0x25, 0x45, 0xcf, 0xc8, 0x92, 0x19, 0x01,
            0x2b, 0x2d, 0x52, 0x84, 0x2b, 0x2b, 0xdd, 0x98, 0xff, 0xc9, 0x72, 0x95, 0x21, 0x23,
            0xf3, 0xf6, 0x6a, 0xda, 0x55, 0xd9, 0xd8, 0x4a, 0x37, 0xe3, 0x3b, 0x2d, 0x23, 0xfe,
            0x38, 0xfd, 0x14, 0xae, 0x06, 0x67, 0x09, 0x16,
        ];
        let mut cache = new_key_cache();
        let (key, nonce) = create_key_nonce(PacketType::Command, Some(0), 0, 0, &shared_iv, &mut cache);
        // Reference key/nonce from the same test (p_id XOR not yet applied —
        // p_id 0 → key unchanged).
        let expected_key: [u8; 16] = [
            0xf3, 0x70, 0xd3, 0x43, 0xe7, 0x78, 0x15, 0x70, 0x7a, 0xff, 0x60, 0x48, 0xfb, 0xd9,
            0xac, 0x6b,
        ];
        let expected_nonce: [u8; 16] = [
            0xb6, 0x33, 0x35, 0x79, 0x31, 0x9b, 0x88, 0x0e, 0x2d, 0x25, 0xef, 0x9c, 0xe9, 0x9e,
            0x77, 0x5c,
        ];
        assert_eq!(key, expected_key);
        assert_eq!(nonce, expected_nonce);
    }

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let mut iv = [0u8; 64];
        for (i, b) in iv.iter_mut().enumerate() {
            *b = i as u8;
        }
        let mut cache = new_key_cache();
        let mut raw = build_packet(
            Direction::C2S,
            Flags::empty(),
            PacketType::Command,
            123,
            7,
            b"clientinit client_nickname=test",
        );
        encrypt(&mut raw, Direction::C2S, PacketType::Command, true, 123, 0, &iv, &mut cache).unwrap();
        assert!(!is_unencrypted(&raw, Direction::C2S).unwrap());
        let content = decrypt(&mut raw, Direction::C2S, PacketType::Command, 123, 0, &iv, &mut cache).unwrap();
        assert_eq!(content, b"clientinit client_nickname=test");
    }

    #[test]
    fn hash_cash_levels() {
        // sha1("abc0") has exactly 1 leading zero bit.
        assert_eq!(get_hash_cash_level("abc", 0), 1);
        // Level increases with more work; sanity check monotonicity.
        let omega = "test";
        let l1 = get_hash_cash_level(omega, 0);
        let mut found = false;
        for o in 0..10_000u64 {
            if get_hash_cash_level(omega, o) > l1 {
                found = true;
                break;
            }
        }
        assert!(found);
    }
}

