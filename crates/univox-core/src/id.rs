//! Platform identifiers and typed IDs (FEATURES.md §1).

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

/// Typed ID wrapper: stringified internally, with a numeric fast path for
/// platforms with numeric keys (TS3) and plain strings (KOOK/OOPZ).
pub struct Id<T> {
    inner: Arc<IdInner>,
    _marker: PhantomData<fn() -> T>,
}

// Manual impl: the derive would add an unwanted `T: Clone` bound.
impl<T> Clone for Id<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            _marker: PhantomData,
        }
    }
}

#[derive(Debug)]
struct IdInner {
    raw: Box<str>,
    numeric: Option<u64>,
}

impl<T> Default for Id<T> {
    fn default() -> Self {
        Self::from_string(String::new())
    }
}

impl<T> Id<T> {
    pub fn from_u64(v: u64) -> Self {
        Self {
            inner: Arc::new(IdInner {
                raw: v.to_string().into_boxed_str(),
                numeric: Some(v),
            }),
            _marker: PhantomData,
        }
    }

    pub fn from_string(s: impl Into<String>) -> Self {
        let s = s.into();
        let numeric = s.parse::<u64>().ok();
        Self {
            inner: Arc::new(IdInner {
                raw: s.into_boxed_str(),
                numeric,
            }),
            _marker: PhantomData,
        }
    }

    pub fn as_str(&self) -> &str {
        &self.inner.raw
    }

    pub fn as_u64(&self) -> Option<u64> {
        self.inner.numeric
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner.raw)
    }
}

impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.inner.raw)
    }
}

impl<T> PartialEq for Id<T> {
    fn eq(&self, other: &Self) -> bool {
        self.inner.raw == other.inner.raw
    }
}

impl<T> Eq for Id<T> {}

impl<T> PartialOrd for Id<T> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<T> Ord for Id<T> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.inner.raw.cmp(&other.inner.raw)
    }
}

impl<T> std::hash::Hash for Id<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.inner.raw.hash(state);
    }
}

impl<T> From<u64> for Id<T> {
    fn from(v: u64) -> Self {
        Self::from_u64(v)
    }
}

impl<T> From<&str> for Id<T> {
    fn from(s: &str) -> Self {
        Self::from_string(s)
    }
}

impl<T> From<String> for Id<T> {
    fn from(s: String) -> Self {
        Self::from_string(s)
    }
}

/// Marker types for the concrete ID aliases.
pub mod tags {
    pub struct Server;
    pub struct Channel;
    pub struct Member;
    pub struct Message;
    pub struct Role;
    pub struct Db;
    pub struct Session;
}

pub type ServerId = Id<tags::Server>;
pub type ChannelId = Id<tags::Channel>;
pub type MemberId = Id<tags::Member>;
pub type MessageId = Id<tags::Message>;
pub type RoleId = Id<tags::Role>;
pub type DbId = Id<tags::Db>;
pub type SessionId = Id<tags::Session>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_ids() {
        let id: ServerId = 42u64.into();
        assert_eq!(id.as_u64(), Some(42));
        assert_eq!(id.as_str(), "42");
        assert_eq!(id.to_string(), "42");
        assert_eq!(id, 42u64.into());
    }

    #[test]
    fn string_ids() {
        let id: MemberId = "abc_123#xyz".into();
        assert_eq!(id.as_u64(), None);
        assert_eq!(id.as_str(), "abc_123#xyz");
        // Numeric-looking strings still parse but compare by string.
        let a: MemberId = "007".into();
        let b: MemberId = "7".into();
        assert_ne!(a, b);
    }
}
