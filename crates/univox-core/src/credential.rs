//! Credentials (FEATURES.md §3): a provider abstraction feeding drivers,
//! plus an optional encrypted-at-rest store trait.

use std::collections::HashMap;
use std::sync::RwLock;

use crate::error::Result;

/// Platform credential kinds (FEATURES.md §3 table).
#[derive(Debug, Clone)]
pub enum Credential {
    /// TS3 client identity string (official format) — see §3 Ts3Identity.
    Ts3Identity { identity: String },
    /// TS3 ServerQuery login (serveradmin / password).
    Ts3QueryLogin { username: String, password: String },
    /// KOOK `Authorization: Bot <token>`.
    KookBotToken { token: String },
    /// KOOK OAuth2 bearer token + refresh token.
    KookOauth2 { access_token: String, refresh_token: Option<String> },
    /// KOOK webhook verification material.
    KookWebhookSecret { verify_token: String, encrypt_key: String },
    /// OOPZ account login (reverse-engineered protocol).
    OopzAccount { account: String, password: String },
    /// Anonymous/insecure platform access (e.g. TS3 without identity).
    Anonymous,
}

impl Credential {
    pub fn platform(&self) -> crate::platform::Platform {
        match self {
            Credential::Ts3Identity { .. } | Credential::Ts3QueryLogin { .. } => {
                crate::platform::Platform::Ts3
            }
            Credential::KookBotToken { .. }
            | Credential::KookOauth2 { .. }
            | Credential::KookWebhookSecret { .. } => crate::platform::Platform::Kook,
            Credential::OopzAccount { .. } => crate::platform::Platform::Oopz,
            Credential::Anonymous => crate::platform::Platform::Ts3,
        }
    }
}

/// Provides (and can refresh) the credential for a session.
#[async_trait::async_trait]
pub trait CredentialProvider: Send + Sync {
    async fn credential(&self) -> Result<Credential>;
    /// Called after a refresh (e.g. renewed tokens) to persist.
    async fn refresh(&self, _credential: Credential) -> Result<()> {
        Ok(())
    }
}

/// In-memory credential store keyed by a string handle (server uid, bot id).
#[derive(Default)]
pub struct InMemoryCredentialStore {
    map: RwLock<HashMap<String, Credential>>,
}

impl InMemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn put(&self, key: impl Into<String>, credential: Credential) {
        self.map.write().unwrap().insert(key.into(), credential);
    }

    pub fn get(&self, key: &str) -> Option<Credential> {
        self.map.read().unwrap().get(key).cloned()
    }

    pub fn remove(&self, key: &str) -> Option<Credential> {
        self.map.write().unwrap().remove(key)
    }
}

/// Persistent store trait (FEATURES.md §3: encrypted on-disk storage is the
/// recommended implementation; callers must persist identities and tokens).
#[async_trait::async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Credential>>;
    async fn put(&self, key: &str, credential: Credential) -> Result<()>;
    async fn remove(&self, key: &str) -> Result<()>;
}

#[async_trait::async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, key: &str) -> Result<Option<Credential>> {
        Ok(InMemoryCredentialStore::get(self, key))
    }
    async fn put(&self, key: &str, credential: Credential) -> Result<()> {
        InMemoryCredentialStore::put(self, key, credential);
        Ok(())
    }
    async fn remove(&self, key: &str) -> Result<()> {
        InMemoryCredentialStore::remove(self, key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_roundtrip() {
        let store = InMemoryCredentialStore::new();
        store.put(
            "my-server",
            Credential::Ts3QueryLogin {
                username: "serveradmin".into(),
                password: "pw".into(),
            },
        );
        match store.get("my-server") {
            Some(Credential::Ts3QueryLogin { username, .. }) => assert_eq!(username, "serveradmin"),
            other => panic!("unexpected: {other:?}"),
        }
        assert!(store.remove("my-server").is_some());
        assert!(store.get("my-server").is_none());
    }
}
