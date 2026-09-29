//! Supported voice platforms (FEATURES.md README table).

use std::fmt;

/// Supported voice platforms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Ts3,
    Kook,
    Oopz,
    /// Reserved; no public SDK as of 2026-09 (FEATURES.md §14.1).
    Ts6,
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Platform::Ts3 => write!(f, "ts3"),
            Platform::Kook => write!(f, "kook"),
            Platform::Oopz => write!(f, "oopz"),
            Platform::Ts6 => write!(f, "ts6"),
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = crate::error::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "ts3" | "teamspeak3" | "teamspeak" => Ok(Platform::Ts3),
            "kook" => Ok(Platform::Kook),
            "oopz" => Ok(Platform::Oopz),
            "ts6" | "teamspeak6" => Ok(Platform::Ts6),
            other => Err(crate::error::Error::InvalidArgument(format!(
                "unknown platform: {other}"
            ))),
        }
    }
}
