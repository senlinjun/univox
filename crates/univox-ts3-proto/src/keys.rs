//! P-256 (prime256v1) key handling in the TeamSpeak "tomcrypt" formats.
//!
//! Private key DER: `SEQ[BitString(1,[0x80]), Integer(32), Integer(x),
//! Integer(y), Integer(d)]`; public key DER omits the private integer and
//! uses `BitString(1,[0])`. The identity payload is the base64 of these DER
//! blobs; the client UID is `base64(sha1(base64(DER_pub)))`.

use base64::Engine as _;
use sha1::{Digest, Sha1};

use crate::error::{Error, Result};

/// Obfuscation table for the official client identity format.
pub const IDENTITY_OBFUSCATION: [u8; 128] = *b"b9dfaa7bee6ac57ac7b65f1094a1c155e747327bc2fe5d51c512023fe54a280201004e90ad1daaae1075d53b7d571c30e063b5a62a4a017bb394833aa0983e6e";

/// A P-256 key pair.
#[derive(Clone)]
pub struct EccKeyPrivP256(pub p256::SecretKey);

/// A P-256 public key.
#[derive(Clone)]
pub struct EccKeyPubP256(pub p256::PublicKey);

// ---- minimal DER helpers ----

fn der_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len <= 0xFF {
        out.push(0x81);
        out.push(len as u8);
    } else {
        out.push(0x82);
        out.push((len >> 8) as u8);
        out.push(len as u8);
    }
}

fn der_integer(bytes: &[u8]) -> Vec<u8> {
    // Strip leading zeros, then prepend one if the high bit is set.
    let mut start = 0;
    while start < bytes.len() - 1 && bytes[start] == 0 {
        start += 1;
    }
    let mag = &bytes[start..];
    let mut out = vec![0x02];
    let needs_pad = mag[0] & 0x80 != 0;
    der_len(mag.len() + usize::from(needs_pad), &mut out);
    if needs_pad {
        out.push(0);
    }
    out.extend_from_slice(mag);
    out
}

fn der_bit_string(unused_bits: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![0x03];
    der_len(content.len() + 1, &mut out);
    out.push(unused_bits);
    out.extend_from_slice(content);
    out
}

fn der_sequence(parts: &[Vec<u8>]) -> Vec<u8> {
    let content: Vec<u8> = parts.iter().flatten().copied().collect();
    let mut out = vec![0x30];
    der_len(content.len(), &mut out);
    out.extend_from_slice(&content);
    out
}

/// The obfuscation transform (an involution): static XOR over the first 100
/// bytes, then SHA1-XOR over the first 20 bytes with the hash of everything
/// after byte 20 up to the first NUL.
fn obfuscate(mut data: Vec<u8>) -> Vec<u8> {
    for i in 0..data.len().min(100) {
        data[i] ^= IDENTITY_OBFUSCATION[i];
    }
    let pos = data[20..]
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(data.len() - 20);
    let hash = Sha1::digest(&data[20..20 + pos]);
    for i in 0..20 {
        data[i] ^= hash[i];
    }
    data
}

/// A parsed DER structure (subset needed for tomcrypt keys).
struct DerSeq {
    /// (tag, content) pairs.
    items: Vec<(u8, Vec<u8>)>,
}

fn der_parse_sequence(data: &[u8]) -> Result<DerSeq> {
    let mut rest = data;
    if rest.first() != Some(&0x30) {
        return Err(Error::Crypto("expected DER sequence".into()));
    }
    rest = &rest[1..];
    let (len, consumed) = parse_der_len(rest)?;
    rest = &rest[consumed..];
    if rest.len() < len {
        return Err(Error::Crypto("DER sequence truncated".into()));
    }
    let mut items = Vec::new();
    let mut cur = &rest[..len];
    while !cur.is_empty() {
        let tag = cur[0];
        let (l, c) = parse_der_len(&cur[1..])?;
        if cur.len() < 1 + c + l {
            return Err(Error::Crypto("DER element truncated".into()));
        }
        items.push((tag, cur[1 + c..1 + c + l].to_vec()));
        cur = &cur[1 + c + l..];
    }
    Ok(DerSeq { items })
}

fn parse_der_len(data: &[u8]) -> Result<(usize, usize)> {
    if data.is_empty() {
        return Err(Error::Crypto("missing DER length".into()));
    }
    let first = data[0] as usize;
    if first < 0x80 {
        Ok((first, 1))
    } else {
        let n = (first & 0x7F) as usize;
        if n == 0 || n > 4 || data.len() < 1 + n {
            return Err(Error::Crypto("bad DER length".into()));
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | data[1 + i] as usize;
        }
        Ok((len, 1 + n))
    }
}

fn integer_bytes(content: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    let take = content.len().min(32);
    out[32 - take..].copy_from_slice(&content[content.len() - take..]);
    out
}

impl EccKeyPrivP256 {
    pub fn create() -> Self {
        Self(p256::SecretKey::random(&mut rand::thread_rng()))
    }

    pub fn from_short(data: &[u8; 32]) -> Result<Self> {
        Ok(Self(
            p256::SecretKey::from_bytes(data.into()).map_err(|_| Error::Crypto("bad key".into()))?,
        ))
    }

    /// Import from the base64 of a tomcrypt DER blob (with or without the
    /// `<counter>V` prefix handled by the caller).
    pub fn import_str(s: &str) -> Result<Self> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|_| Error::Crypto("bad identity base64".into()))?;
        Self::from_tomcrypt(&der)
    }

    /// Decode the tomcrypt ASN.1 structure: `SEQ[BitString, Integer(32),
    /// Integer(x), Integer(y), Integer(d)]` (private) or without `d`
    /// (public — only usable for the public key).
    pub fn from_tomcrypt(data: &[u8]) -> Result<Self> {
        let seq = der_parse_sequence(data)?;
        if seq.items.len() < 5 || seq.items[0].0 != 0x03 {
            return Err(Error::Crypto("not a private tomcrypt key".into()));
        }
        // BitString content: [unused_bits, data...]; the MSB of the first
        // data byte marks a contained private key.
        let bs = &seq.items[0].1;
        if bs.len() < 2 || bs[1] & 0x80 == 0 {
            return Err(Error::Crypto("bit string does not mark a private key".into()));
        }
        let d = integer_bytes(&seq.items[4].1);
        Self::from_short(&d)
    }

    /// The 32-byte private scalar.
    pub fn to_short(&self) -> [u8; 32] {
        self.0.to_bytes().into()
    }

    /// base64 of the tomcrypt DER private key (the identity payload).
    pub fn to_ts(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.to_tomcrypt())
    }

    pub fn to_tomcrypt(&self) -> Vec<u8> {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let pub_point = self.0.public_key().to_encoded_point(false);
        let x = pub_point.x().unwrap();
        let y = pub_point.y().unwrap();
        let d = self.0.to_bytes();
        der_sequence(&[
            der_bit_string(7, &[0x80]),
            der_integer(&[32]),
            der_integer(x),
            der_integer(y),
            der_integer(&d),
        ])
    }

    /// Store as the obfuscated official-client identity (without counter).
    pub fn to_ts_obfuscated(&self) -> String {
        let data = self.to_ts().into_bytes();
        base64::engine::general_purpose::STANDARD.encode(obfuscate(data))
    }

    /// Deobfuscate an obfuscated identity string: hash XOR first (using the
    /// obfuscated bytes — the hash only covers bytes ≥ 20 which the hash XOR
    /// does not touch), then the static XOR.
    pub fn from_ts_obfuscated(s: &str) -> Result<Self> {
        let mut data = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|_| Error::Crypto("bad obfuscated identity base64".into()))?;
        if data.len() < 20 {
            return Err(Error::Crypto("obfuscated key too short".into()));
        }
        let pos = data[20..]
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(data.len() - 20);
        let hash = Sha1::digest(&data[20..20 + pos]);
        for i in 0..20 {
            data[i] ^= hash[i];
        }
        for i in 0..data.len().min(100) {
            data[i] ^= IDENTITY_OBFUSCATION[i];
        }
        let ascii = String::from_utf8(data)
            .map_err(|_| Error::Crypto("deobfuscated key is not ascii".into()))?;
        Self::from_ts(&ascii)
    }

    /// From base64 encoded tomcrypt key.
    pub fn from_ts(data: &str) -> Result<Self> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(data.trim())
            .map_err(|_| Error::Crypto("bad key base64".into()))?;
        Self::from_tomcrypt(&der)
    }

    pub fn to_pub(&self) -> EccKeyPubP256 {
        EccKeyPubP256(self.0.public_key())
    }

    /// ECDSA-P256/SHA256 signature in DER encoding.
    pub fn sign(&self, data: &[u8]) -> Vec<u8> {
        use p256::ecdsa::signature::Signer;
        let key = p256::ecdsa::SigningKey::from(&self.0);
        let sig: p256::ecdsa::DerSignature = key.sign(data);
        sig.as_bytes().to_vec()
    }

    pub fn ecdsa_verify(pub_key: &EccKeyPubP256, data: &[u8], sig_der: &[u8]) -> Result<()> {
        use p256::ecdsa::signature::Verifier;
        let vk = p256::ecdsa::VerifyingKey::from(&pub_key.0);
        let sig = p256::ecdsa::Signature::from_der(sig_der)
            .map_err(|_| Error::Crypto("bad ECDSA signature encoding".into()))?;
        vk.verify(data, &sig)
            .map_err(|_| Error::Crypto("ECDSA signature verification failed".into()))
    }
}

impl EccKeyPubP256 {
    pub fn from_ts(s: &str) -> Result<Self> {
        let der = base64::engine::general_purpose::STANDARD
            .decode(s.trim())
            .map_err(|_| Error::Crypto("bad key base64".into()))?;
        Self::from_tomcrypt(&der)
    }

    pub fn from_tomcrypt(data: &[u8]) -> Result<Self> {
        let seq = der_parse_sequence(data)?;
        if seq.items.len() < 4 || seq.items[0].0 != 0x03 {
            return Err(Error::Crypto("not a tomcrypt public key".into()));
        }
        let x = integer_bytes(&seq.items[2].1);
        let y = integer_bytes(&seq.items[3].1);
        let point = p256::EncodedPoint::from_affine_coordinates(
            (&x).into(),
            (&y).into(),
            false,
        );
        let pk = p256::PublicKey::try_from(&point)
            .map_err(|_| Error::Crypto("invalid public key point".into()))?;
        Ok(Self(pk))
    }

    pub fn to_ts(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.to_tomcrypt())
    }

    pub fn to_tomcrypt(&self) -> Vec<u8> {
        use p256::elliptic_curve::sec1::ToEncodedPoint;
        let point = self.0.to_encoded_point(false);
        let x = point.x().unwrap();
        let y = point.y().unwrap();
        der_sequence(&[
            der_bit_string(7, &[0x00]),
            der_integer(&[32]),
            der_integer(x),
            der_integer(y),
        ])
    }

    /// uid = base64(sha1(base64(DER))) — the `client_unique_identifier`.
    pub fn get_uid(&self) -> String {
        let ts = self.to_ts();
        let digest = Sha1::digest(ts.as_bytes());
        base64::engine::general_purpose::STANDARD.encode(digest)
    }

    pub fn get_uid_no_base64(&self) -> Vec<u8> {
        let ts = self.to_ts();
        Sha1::digest(ts.as_bytes()).to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vectors from the tsclientlib reference implementation.
    const TEST_PRIV_KEY: &str = "MG8DAgeAAgEgAiEA6rtKxDn/o/Bo50rNtAE5Ph3h2RKLHQ0gbFkvm2yA79kCIQCrfzAZts/vHP+3MOetKLjNnpZXt4c6U3UB4gWLKR4H9AIgYTyJofmztcTBjq3KZcDdxu+G4RPVwE5vg8VaN2jbQao=";
    const TEST_UID: &str = "test/9PZ9vww/Bpf5vJxtJhpz80=";

    #[test]
    fn parse_ts_base64() {
        let key = EccKeyPrivP256::import_str(TEST_PRIV_KEY).unwrap();
        assert_eq!(key.to_pub().get_uid(), TEST_UID);
    }

    #[test]
    fn to_tomcrypt_roundtrip() {
        let key = EccKeyPrivP256::create();
        let der = key.to_tomcrypt();
        let reparsed = EccKeyPrivP256::from_tomcrypt(&der).unwrap();
        assert_eq!(key.to_short(), reparsed.to_short());
        assert_eq!(key.to_pub().get_uid(), reparsed.to_pub().get_uid());
    }

    #[test]
    fn uid_is_sha1_of_b64_string() {
        let key = EccKeyPrivP256::create();
        let uid = key.to_pub().get_uid();
        let digest = Sha1::digest(key.to_pub().to_ts().as_bytes());
        assert_eq!(uid, base64::engine::general_purpose::STANDARD.encode(digest));
        assert_eq!(uid.len(), 28); // 20 bytes → base64 with padding
    }

    #[test]
    fn obfuscated_roundtrip() {
        // The obfuscation transform is an involution: applying it to the
        // obfuscated form yields the base64 DER again.
        let key = EccKeyPrivP256::create();
        let obf = key.to_ts_obfuscated();
        let reparsed = EccKeyPrivP256::from_ts_obfuscated(&obf).unwrap();
        assert_eq!(key.to_short(), reparsed.to_short());
        assert_eq!(key.to_pub().get_uid(), reparsed.to_pub().get_uid());
    }
}
