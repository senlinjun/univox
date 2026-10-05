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

    /// Highest counter ever tried (kept across upgrades).
    pub fn max_counter(&self) -> u64 {
        self.max_counter
    }

    /// Official serialization (FEATURES.md: compatible with the official
    /// client identity format).
    pub fn to_string_official(&self) -> String {
        format!("{}V{}", self.counter, self.key.to_ts())
    }

    /// Serialize as the tsclientlib/tsproto JSON format:
    /// `{"key": "<base64>", "counter": N, "max_counter": M}`.
    ///
    /// The key is base64 of the raw 32-byte private scalar, matching
    /// tsproto 0.2's `serialize_id_key` byte for byte, so identity strings
    /// can be passed through from/to consumers unchanged.
    pub fn to_tsclientlib_json(&self) -> String {
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        serde_json::json!({
            "key": engine.encode(self.key.to_short()),
            "counter": self.counter,
            "max_counter": self.max_counter,
        })
        .to_string()
    }

    /// Parse the tsclientlib/tsproto JSON identity format (see
    /// [`Self::to_tsclientlib_json`]).
    ///
    /// Lenient like tsproto's own `import_str`: the key may be the raw
    /// 32-byte scalar (what tsproto writes) or the base64 tomcrypt DER blob
    /// (what the official string format carries); `current_counter` is
    /// accepted as an alias for `counter`; a missing `max_counter` defaults
    /// to `counter`.
    pub fn from_tsclientlib_json(s: &str) -> Result<Self> {
        let v: serde_json::Value = serde_json::from_str(s.trim())
            .map_err(|e| Error::Crypto(format!("identity json: {e}")))?;
        let key_b64 = v
            .get("key")
            .and_then(|k| k.as_str())
            .ok_or_else(|| Error::Crypto("identity json: missing \"key\"".into()))?;
        use base64::Engine;
        let engine = base64::engine::general_purpose::STANDARD;
        let raw = engine
            .decode(key_b64.trim())
            .map_err(|_| Error::Crypto("identity json: bad key base64".into()))?;
        let key = match <&[u8; 32]>::try_from(raw.as_slice()) {
            Ok(short) => EccKeyPrivP256::from_short(short)?,
            Err(_) => EccKeyPrivP256::from_tomcrypt(&raw)?,
        };
        let counter = v
            .get("counter")
            .or_else(|| v.get("current_counter"))
            .and_then(|c| c.as_u64())
            .unwrap_or(0);
        let max_counter = v
            .get("max_counter")
            .and_then(|c| c.as_u64())
            .unwrap_or(counter);
        Ok(Self {
            key,
            counter,
            max_counter,
        })
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

    /// The reference identity from tsclientlib (see `parse_official_format`).
    const REFERENCE: &str = "2792354VMG8DAgeAAgEgAiEA6rtKxDn/o/Bo50rNtAE5Ph3h2RKLHQ0gbFkvm2yA79kCIQCrfzAZts/vHP+3MOetKLjNnpZXt4c6U3UB4gWLKR4H9AIgYTyJofmztcTBjq3KZcDdxu+G4RPVwE5vg8VaN2jbQao=";

    #[test]
    fn tsclientlib_json_roundtrip() {
        use base64::Engine;
        let ident = Identity::parse(REFERENCE).unwrap();
        let json = ident.to_tsclientlib_json();
        // tsproto 0.2's Identity serde: key = base64(to_short()), and the
        // field names are key/counter/max_counter.
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v.get("counter").unwrap().as_u64(), Some(2_792_354));
        let key = base64::engine::general_purpose::STANDARD
            .decode(v.get("key").unwrap().as_str().unwrap())
            .unwrap();
        assert_eq!(key.as_slice(), &ident.key().to_short());
        // Parsing it back restores the same identity.
        let re = Identity::from_tsclientlib_json(&json).unwrap();
        assert_eq!(re.counter(), ident.counter());
        assert_eq!(re.uid(), ident.uid());
        assert_eq!(re.level(), ident.level());
    }

    #[test]
    fn tsclientlib_json_accepts_tomcrypt_key() {
        use base64::Engine;
        let ident = Identity::parse(REFERENCE).unwrap();
        let key = base64::engine::general_purpose::STANDARD.encode(ident.key().to_tomcrypt());
        let json = format!(
            r#"{{"key":"{key}","counter":2792354,"max_counter":2792354}}"#
        );
        let re = Identity::from_tsclientlib_json(&json).unwrap();
        assert_eq!(re.uid(), "test/9PZ9vww/Bpf5vJxtJhpz80=");
        assert_eq!(re.counter(), 2_792_354);
    }

    #[test]
    fn tsclientlib_json_lenient_fields() {
        let ident = Identity::parse(REFERENCE).unwrap();
        // `current_counter` spelling and missing max_counter are accepted.
        let json = format!(
            r#"{{"key":"{}","current_counter":2792354}}"#,
            {
                use base64::Engine;
                base64::engine::general_purpose::STANDARD.encode(ident.key().to_short())
            }
        );
        let re = Identity::from_tsclientlib_json(&json).unwrap();
        assert_eq!(re.counter(), 2_792_354);
        assert_eq!(re.max_counter(), 2_792_354);
        assert_eq!(re.uid(), ident.uid());
    }
}
