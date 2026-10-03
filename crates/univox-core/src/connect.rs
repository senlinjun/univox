//! Connection configuration (FEATURES.md §2.1): builder-style options with
//! a typed extension map for platform-specific additions.

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::time::Duration;

use crate::credential::Credential;
use crate::error::Result;
use crate::id::ChannelId;
use crate::ratelimit::RateLimits;

/// Reconnect policy (FEATURES.md §2.1/§2.4).
#[derive(Debug, Clone)]
pub struct ReconnectPolicy {
    pub max_attempts: u32,
    /// Exponential backoff base; each attempt waits `base * 2^n` + jitter.
    pub base_delay: Duration,
    pub max_delay: Duration,
    pub jitter: f64,
    /// Re-establish prior state (channel, mutes, subscriptions) after resume.
    pub restore_state: bool,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 5,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            jitter: 0.2,
            restore_state: true,
        }
    }
}

/// Compute the delay for attempt `n` (0-based).
impl ReconnectPolicy {
    pub fn delay_for(&self, n: u32) -> Duration {
        let exp = self
            .base_delay
            .saturating_mul(1u32.checked_shl(n.min(16)).unwrap_or(u32::MAX));
        let capped = exp.min(self.max_delay);
        if self.jitter > 0.0 {
            let jitter_span = capped.as_secs_f64() * self.jitter;
            // Deterministic pseudo-jitter from attempt number — callers
            // needing real randomness can wrap this policy.
            let factor = 1.0 + ((n as f64 * 0.618_033) % 1.0 - 0.5) * 2.0 * self.jitter;
            Duration::from_secs_f64((capped.as_secs_f64() + jitter_span * (factor - 1.0)).max(0.0))
        } else {
            capped
        }
    }
}

/// Network configuration (FEATURES.md §12).
#[derive(Debug, Clone, Default)]
pub struct NetworkConfig {
    /// Local address to bind (host only; port 0 = any).
    pub bind_address: Option<String>,
    /// SOCKS5/HTTP proxy URL, if the transport supports it.
    pub proxy: Option<String>,
    pub connect_timeout: Option<Duration>,
    /// Custom DNS resolver endpoint (e.g. "https://1.1.1.1/dns-query").
    pub dns_resolver: Option<String>,
    pub ipv6: bool,
}

/// Bookkeeping scope (FEATURES.md §4, "可关闭").
///
/// Defaults to enabled — the mirror is a core feature users opt *out* of
/// (consistent with [`crate::bookkeeping::BookConfig::default`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookkeepingConfig {
    pub enabled: bool,
    pub member_states: bool,
}

impl Default for BookkeepingConfig {
    fn default() -> Self {
        Self { enabled: true, member_states: true }
    }
}

/// Initial member state after connecting (FEATURES.md §2.1).
#[derive(Debug, Clone, Default)]
pub struct InitialState {
    pub input_muted: bool,
    pub output_muted: bool,
    pub away: bool,
    pub away_message: Option<String>,
}

/// Connection options, built with the builder pattern.
#[derive(Debug, Clone, Default)]
pub struct ConnectOptions {
    /// `host:port`, invite code or connection string (§11 platform parsers).
    pub address: String,
    pub credential: Option<Credential>,
    pub nickname: Option<String>,
    pub initial_channel: Option<InitialChannel>,
    pub initial_state: InitialState,
    pub network: NetworkConfig,
    pub reconnect: ReconnectPolicy,
    pub bookkeeping: BookkeepingConfig,
    /// Per-session rate limit overrides.
    pub rate_limits: Option<RateLimits>,
    /// Platform extension options (e.g. `univox_ts3::Ts3ConnectOptions`).
    pub extensions: BTreeMap<TypeId, std::sync::Arc<dyn Any + Send + Sync>>,
}

/// Target channel after connecting: by id or by path with optional password.
#[derive(Debug, Clone, PartialEq)]
pub enum InitialChannel {
    Id(ChannelId),
    /// `/parent/child` style path.
    Path(String),
    /// Path + password.
    PathWithPassword { path: String, password: String },
}

impl Default for InitialChannel {
    fn default() -> Self {
        InitialChannel::Path(String::new())
    }
}

impl ConnectOptions {
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            ..Default::default()
        }
    }

    pub fn credential(mut self, c: Credential) -> Self {
        self.credential = Some(c);
        self
    }

    pub fn nickname(mut self, n: impl Into<String>) -> Self {
        self.nickname = Some(n.into());
        self
    }

    pub fn initial_channel(mut self, c: InitialChannel) -> Self {
        self.initial_channel = Some(c);
        self
    }

    /// Configure the bookkeeping mirror (FEATURES.md §4).
    pub fn bookkeeping(mut self, cfg: BookkeepingConfig) -> Self {
        self.bookkeeping = cfg;
        self
    }

    pub fn with_extension<E: Any + Send + Sync>(mut self, ext: E) -> Self {
        self.extensions
            .insert(TypeId::of::<E>(), std::sync::Arc::new(ext));
        self
    }

    pub fn extension<E: Any + Send + Sync>(&self) -> Option<&E> {
        self.extensions.get(&TypeId::of::<E>()).and_then(|b| b.downcast_ref::<E>())
    }
}

/// A type-erased platform extension getter used by drivers.
pub trait HasExtensions {
    fn extensions(&self) -> &BTreeMap<TypeId, std::sync::Arc<dyn Any + Send + Sync>>;

    fn ext<E: Any + Send + Sync>(&self) -> Option<&E> {
        self.extensions().get(&TypeId::of::<E>()).and_then(|b| b.downcast_ref::<E>())
    }
}

impl HasExtensions for ConnectOptions {
    fn extensions(&self) -> &BTreeMap<TypeId, std::sync::Arc<dyn Any + Send + Sync>> {
        &self.extensions
    }
}

/// A connect request handed to [`crate::session::SessionManager`]: picks
/// the driver and tags the session.
#[derive(Debug, Clone)]
pub struct SessionRequest {
    pub platform: crate::platform::Platform,
    pub options: ConnectOptions,
    pub tag: Option<String>,
}

impl SessionRequest {
    pub fn new(platform: crate::platform::Platform, options: ConnectOptions) -> Self {
        Self {
            platform,
            options,
            tag: None,
        }
    }

    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.tag = Some(tag.into());
        self
    }
}

/// Capability set declared by a driver at startup (G4).
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub platform: crate::platform::Platform,
    pub audio: crate::audio::AudioCapability,
    pub message_history: bool,
    pub message_edit: bool,
    pub message_delete: bool,
    pub reactions: bool,
    pub mentions: bool,
    pub channel_management: bool,
    pub server_management: bool,
    pub role_management: bool,
    pub file_transfer: bool,
    pub kicks: bool,
    pub bans: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builder_and_extensions() {
        #[derive(Debug)]
        struct Ts3Opts {
            version_spoof: String,
        }
        let opts = ConnectOptions::new("127.0.0.1:9987")
            .nickname("bot")
            .with_extension(Ts3Opts {
                version_spoof: "3.6.2".into(),
            })
            .initial_channel(InitialChannel::Path("/Lobby".into()));
        assert_eq!(opts.nickname.as_deref(), Some("bot"));
        let ts3: &Ts3Opts = opts.extension().expect("ts3 extension");
        assert_eq!(ts3.version_spoof, "3.6.2");
        assert!(opts
            .initial_channel
            .as_ref()
            .unwrap()
            == &InitialChannel::Path("/Lobby".into()));
    }

    #[test]
    fn backoff_growth() {
        let policy = ReconnectPolicy {
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(5000),
            jitter: 0.0,
            ..Default::default()
        };
        assert_eq!(policy.delay_for(0), Duration::from_millis(100));
        assert_eq!(policy.delay_for(1), Duration::from_millis(200));
        assert_eq!(policy.delay_for(2), Duration::from_millis(400));
        assert_eq!(policy.delay_for(10), Duration::from_millis(5000));
    }
}
