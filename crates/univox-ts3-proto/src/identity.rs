//! TS3 identity: P-256 key pair + hash-cash counter (FEATURES.md §3).
//!
//! Official string format: `<counter>V<base64(tomcrypt DER private key)>`.
//! The security level is the number of leading zero bits of
//! `sha1(base64(pubkey-DER) || counter)`.

use crate::crypto::get_hash_cash_level;
use crate::error::{Error, Result};
use crate::keys::EccKeyPrivP256;

#[derive(Clone)]
pub struct Identity {
    key: EccKeyPrivP256,
    counter: u64,
    /// Highest counter ever tried (kept across upgrades).
    max_counter: u64,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity")
            .field("counter", &self.counter)
            .field("max_counter", &self.max_counter)
            .field("uid", &self.uid())
            .finish()
    }
}

impl Identity {
    /// New identity with security level 8 (the official default).
    pub fn create() -> Self {
        let mut id = Self::new(EccKeyPrivP256::create(), 0);
        id.upgrade_level(8);
        id
    }

    pub fn new(key: EccKeyPrivP256, counter: u64) -> Self {
        Self {
            key,
            counter,
            max_counter: counter,
        }
    }

    /// Parse an identity string: either `<counter>V<key>` or just a key
    /// (counter 0).
    pub fn parse(s: &str) -> Result<Self> {
        let s = s.trim();
        if let Some(sep) = s.find('V') {
            if let Ok(counter) = s[..sep].parse::<u64>() {
                let key = EccKeyPrivP256::import_str(&s[sep + 1..])?;
                return Ok(Self::new(key, counter));
            }
        }
        // No (valid) counter block: the whole string is the key.
        Ok(Self::new(EccKeyPrivP256::import_str(s)?, 0))
    }

    pub fn key(&self) -> &EccKeyPrivP256 {
        &self.key
    }

    pub fn counter(&self) -> u64 {
        self.counter
    }

    /// Official serialization (FEATURES.md: compatible with the official
    /// client identity format).
    pub fn to_string_official(&self) -> String {
        format!("{}V{}", self.counter, self.key.to_ts())
    }

    /// The `client_unique_identifier` derived from this identity.
    pub fn uid(&self) -> String {
        self.key.to_pub().get_uid()
    }

    /// Current hash-cash security level (0..=40+).
    pub fn level(&self) -> u8 {
        get_hash_cash_level(&self.key.to_pub().to_ts(), self.counter)
    }

    /// Improve the level by searching a counter with the needed leading
    /// zero bits. CPU-bound: run it off the async thread.
    pub fn upgrade_level(&mut self, target: u8) {
        let omega = self.key.to_pub().to_ts();
        let mut offset = self.max_counter;
        while offset < u64::MAX && get_hash_cash_level(&omega, offset) < target {
            offset += 1;
        }
        self.counter = offset;
        self.max_counter = offset;
    }

    /// Blocking upgrade for use with `spawn_blocking`.
    pub fn upgrade_level_blocking(mut self, target: u8) -> Self {
        self.upgrade_level(target);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_official_format() {
        // Reference identity from tsclientlib; counter 2792354 → level 21.
        let ident = Identity::parse("2792354VMG8DAgeAAgEgAiEA6rtKxDn/o/Bo50rNtAE5Ph3h2RKLHQ0gbFkvm2yA79kCIQCrfzAZts/vHP+3MOetKLjNnpZXt4c6U3UB4gWLKR4H9AIgYTyJofmztcTBjq3KZcDdxu+G4RPVwE5vg8VaN2jbQao=").unwrap();
        assert_eq!(ident.counter(), 2792354);
        assert_eq!(ident.level(), 21);
        assert_eq!(ident.uid(), "test/9PZ9vww/Bpf5vJxtJhpz80=");
        // Roundtrip.
        let ser = ident.to_string_official();
        let reparsed = Identity::parse(&ser).unwrap();
        assert_eq!(reparsed.counter(), 2792354);
        assert_eq!(reparsed.uid(), ident.uid());
    }

    #[test]
    fn parse_without_counter() {
        let ident = Identity::parse("MG8DAgeAAgEgAiEA6rtKxDn/o/Bo50rNtAE5Ph3h2RKLHQ0gbFkvm2yA79kCIQCrfzAZts/vHP+3MOetKLjNnpZXt4c6U3UB4gWLKR4H9AIgYTyJofmztcTBjq3KZcDdxu+G4RPVwE5vg8VaN2jbQao=").unwrap();
        assert_eq!(ident.counter(), 0);
        assert_eq!(ident.level(), 0);
    }

    #[test]
    fn create_default_level_8() {
        let ident = Identity::create();
        assert!(ident.level() >= 8, "default identity level {}", ident.level());
    }

    #[test]
    fn upgrade_level() {
        let mut ident = Identity::new(EccKeyPrivP256::create(), 0);
        ident.upgrade_level(10);
        assert!(ident.level() >= 10);
        // Upgrading to a lower level keeps the current counter.
        let before = ident.counter();
        ident.upgrade_level(5);
        assert_eq!(ident.counter(), before);
    }
}
