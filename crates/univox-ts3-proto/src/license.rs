//! TeamSpeak license chain parsing and public key derivation (new protocol).
//!
//! The `lic` argument of `initivexpand2` carries a chain of license blocks;
//! each block contributes an Edwards-point operation that derives the
//! server's ephemeral Curve25519 public key from a fixed root point.

use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use sha2::{Digest, Sha512};

use crate::crypto::ROOT_KEY;
use crate::error::{Error, Result};

const BLOCK_MIN_LEN: usize = 42;
const BLOCK_TYPE_OFFSET: usize = 33;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Intermediate,
    Website,
    Server,
    Code,
    Ts5Server,
    Ephemeral,
}

impl BlockKind {
    fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => BlockKind::Intermediate,
            1 => BlockKind::Website,
            2 => BlockKind::Server,
            3 => BlockKind::Code,
            8 => BlockKind::Ts5Server,
            32 => BlockKind::Ephemeral,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct Block {
    len: usize,
}

/// A parsed license chain (`l` argument, base64-decoded).
#[derive(Debug, Clone)]
pub struct Licenses {
    pub data: Vec<u8>,
    blocks: Vec<Block>,
}

impl Licenses {
    /// Parse without validity checks (we do not verify block timestamps —
    /// the outer signature over `l` is checked instead).
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        if data.is_empty() {
            return Err(Error::Crypto("empty license data".into()));
        }
        let version = data[0];
        if version != 0 && version != 1 {
            return Err(Error::Crypto(format!("unsupported license version {version}")));
        }
        let mut blocks = Vec::new();
        let mut offset = 1;
        while data.len() > offset {
            if blocks.len() >= 8 {
                return Err(Error::Crypto("too many license blocks".into()));
            }
            let len = Self::block_len(&data[offset..])?;
            blocks.push(Block { len });
            offset += len;
        }
        Ok(Licenses { data, blocks })
    }

    fn block_len(data: &[u8]) -> Result<usize> {
        if data.len() < BLOCK_MIN_LEN {
            return Err(Error::Crypto("license block too short".into()));
        }
        if data[0] != 0 {
            return Err(Error::Crypto(format!("wrong license key kind {}", data[0])));
        }
        let kind = BlockKind::from_u8(data[BLOCK_TYPE_OFFSET])
            .ok_or_else(|| Error::Crypto("unknown license block type".into()))?;
        let rest = &data[BLOCK_MIN_LEN..];
        let extra = match kind {
            BlockKind::Intermediate => {
                if rest.len() < 5 {
                    return Err(Error::Crypto("intermediate block too short".into()));
                }
                let len = rest[4..]
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or_else(|| Error::Crypto("non-terminated issuer".into()))?;
                5 + len
            }
            BlockKind::Server => {
                if rest.len() < 6 {
                    return Err(Error::Crypto("server block too short".into()));
                }
                let len = rest[5..]
                    .iter()
                    .position(|&b| b == 0)
                    .ok_or_else(|| Error::Crypto("non-terminated issuer".into()))?;
                6 + len
            }
            // Website/Code blocks carry a single length byte.
            BlockKind::Website | BlockKind::Code => {
                if rest.is_empty() {
                    return Err(Error::Crypto("block too short".into()));
                }
                1 + rest[0] as usize
            }
            BlockKind::Ts5Server => {
                return Err(Error::Crypto("ts5 license blocks unsupported".into()));
            }
            // Ephemeral key blocks have no extra data.
            BlockKind::Ephemeral => 0,
        };
        Ok(BLOCK_MIN_LEN + extra)
    }

    /// Derive the final public key from the TS3 root point.
    pub fn derive_public_key(&self) -> Result<EdwardsPoint> {
        let root = CompressedEdwardsY(ROOT_KEY)
            .decompress()
            .ok_or_else(|| Error::Crypto("invalid root key".into()))?;
        self.derive_public_key_from(root)
    }

    /// Derive the final public key from an explicit root point.
    pub fn derive_public_key_from(&self, root: EdwardsPoint) -> Result<EdwardsPoint> {
        let mut last = root;
        let mut offset = 1;
        for block in &self.blocks {
            let data = &self.data[offset..offset + block.len];
            // Public key of this block.
            let mut pk = [0u8; 32];
            pk.copy_from_slice(&data[1..33]);
            let pub_key = CompressedEdwardsY(pk)
                .decompress()
                .ok_or_else(|| Error::Crypto("invalid block public key".into()))?;
            // clamped scalar from sha512 of everything after the kind byte.
            let mut hash_key = Sha512::digest(&data[1..]);
            hash_key[0] &= 248;
            hash_key[31] &= 63;
            hash_key[31] |= 64;
            let scalar = Scalar::from_bytes_mod_order(hash_key[..32].try_into().unwrap());
            last = pub_key * scalar + last;
            offset += block.len;
        }
        Ok(last)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    /// Test license from tsproto (`shared_iv31` test) — must parse and
    /// derive a key that produces the known shared IV.
    #[test]
    fn parse_and_derive_reference_license() {
        let data = base64::engine::general_purpose::STANDARD
            .decode("AQA1hUFJiiSs0wFXkYuPUJVcDa6XCrZTcsvkB0Ffzz4CmwIITRXgCqeTYAcAAAAgQW5vbnltb3VzAAC4R+5mos+UQ/KCbkpQLMI5WRp4wkQu8e5PZY4zU+/FlyAJwaE8CcJJ/A==")
            .unwrap();
        let licenses = Licenses::parse(data).unwrap();
        let key = licenses.derive_public_key().unwrap();

        // From the reference test: deriving with client ek
        // b04ea1d95c7264df0de8b36baa7ca15f7571f51fa054b55127088edd963d6e79
        // yields shared iv sha512 ⊕ alpha ⊕ beta = expected_xored iv.
        let ek = Scalar::from_bytes_mod_order([
            0xb0, 0x4e, 0xa1, 0xd9, 0x5c, 0x72, 0x64, 0xdf, 0x0d, 0xe8, 0xb3, 0x6b, 0xaa, 0x7c,
            0xa1, 0x5f, 0x75, 0x71, 0xf5, 0x1f, 0xa0, 0x54, 0xb5, 0x51, 0x27, 0x08, 0x8e, 0xdd,
            0x96, 0x3d, 0x6e, 0x79,
        ]);
        let shared = (key * ek).compress().to_bytes();
        let digest = Sha512::digest(shared);

        let alpha_b64 = "Jkxq1wIvvhzaCA==";
        let beta_b64 = "wU5T/MM6toW6Wge9th7VlTlzVZ9JDWypw2P9migfc25pjGP2Tj7Hm6rJpmKeHRr08Ch7BEAR";
        let alpha = base64::engine::general_purpose::STANDARD.decode(alpha_b64).unwrap();
        let beta = base64::engine::general_purpose::STANDARD.decode(beta_b64).unwrap();

        let mut iv = [0u8; 64];
        iv.copy_from_slice(&digest);
        for i in 0..10 {
            iv[i] ^= alpha[i];
        }
        for i in 0..54 {
            iv[i + 10] ^= beta[i];
        }
        let expected: [u8; 64] = [
            0x7e, 0x34, 0xc4, 0xdf, 0x0a, 0x5d, 0xbb, 0xac, 0xc9, 0x2f, 0xd1, 0xa7, 0xd2, 0x48,
            0x6c, 0x2e, 0xa2, 0xf4, 0x17, 0x97, 0x85, 0x25, 0x45, 0xcf, 0xc8, 0x92, 0x19, 0x01,
            0x2b, 0x2d, 0x52, 0x84, 0x2b, 0x2b, 0xdd, 0x98, 0xff, 0xc9, 0x72, 0x95, 0x21, 0x23,
            0xf3, 0xf6, 0x6a, 0xda, 0x55, 0xd9, 0xd8, 0x4a, 0x37, 0xe3, 0x3b, 0x2d, 0x23, 0xfe,
            0x38, 0xfd, 0x14, 0xae, 0x06, 0x67, 0x09, 0x16,
        ];
        assert_eq!(iv, expected);
    }
}
