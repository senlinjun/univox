//! Server state bookkeeping (FEATURES.md §4): an in-memory mirror of the
//! server state (channel tree, members, roles, voice states), driven by
//! property-level changes, with query helpers.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use crate::id::{ChannelId, MemberId, RoleId, ServerId};
use crate::model::{Channel, Member, MemberState, Role, SelfMember, Server, VoiceState};

/// A single property-level change (G2: attribute-level diffing).
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyChange {
    pub key: String,
    pub old: Option<String>,
    pub new: String,
}

impl PropertyChange {
    pub fn added(key: impl Into<String>, new: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            old: None,
            new: new.into(),
        }
    }

    pub fn changed(key: impl Into<String>, old: impl Into<String>, new: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            old: Some(old.into()),
            new: new.into(),
        }
    }
}

/// Diff two rows of properties; returns the added/changed entries.
pub fn diff_rows(
    old: &BTreeMap<String, String>,
    new: &BTreeMap<String, String>,
) -> Vec<PropertyChange> {
    let mut out = Vec::new();
    for (k, v) in new {
        match old.get(k) {
            None => out.push(PropertyChange::added(k.clone(), v.clone())),
            Some(old_v) if old_v != v => {
                out.push(PropertyChange::changed(k.clone(), old_v.clone(), v.clone()))
            }
            _ => {}
        }
    }
    out
}

/// Bookkeeping scope (what the mirror tracks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BookConfig {
    pub enabled: bool,
    /// Track member states (voice states, mutes, ...) — costs memory.
    pub member_states: bool,
}

impl Default for BookConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            member_states: true,
        }
    }
}

/// In-memory server state mirror.
#[derive(Debug, Default)]
pub struct BookInner {
    pub server: Option<Server>,
    pub channels: BTreeMap<ChannelId, Channel>,
    pub members: BTreeMap<MemberId, Member>,
    pub member_states: BTreeMap<MemberId, MemberState>,
    pub roles: BTreeMap<RoleId, Role>,
    pub voice_states: BTreeMap<MemberId, VoiceState>,
    pub self_member: SelfMember,
}

/// Thread-safe handle around [`BookInner`]. Cheap to clone.
#[derive(Clone, Default)]
pub struct Book {
    inner: std::sync::Arc<RwLock<BookInner>>,
    config: std::sync::Arc<BookConfig>,
    poisoned: std::sync::Arc<AtomicBool>,
}

impl Book {
    pub fn new(config: BookConfig) -> Self {
        Self {
            inner: Default::default(),
            config: std::sync::Arc::new(config),
            poisoned: Default::default(),
        }
    }

    pub fn config(&self) -> &BookConfig {
        &self.config
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled && !self.poisoned.load(Ordering::Relaxed)
    }

    /// Read access to the mirror. The lock is held for the closure only.
    pub fn with<R>(&self, f: impl FnOnce(&BookInner) -> R) -> Option<R> {
        if !self.enabled() {
            return None;
        }
        self.inner.read().ok().map(|book| f(&book))
    }

    /// Write access for drivers applying property-level changes.
    pub fn with_mut<R>(&self, f: impl FnOnce(&mut BookInner) -> R) -> Option<R> {
        if !self.config.enabled || self.poisoned.load(Ordering::Relaxed) {
            return None;
        }
        match self.inner.write() {
            Ok(mut book) => Some(f(&mut book)),
            Err(_) => {
                // A writer panicked earlier: disable bookkeeping rather than
                // corrupt the session.
                self.poisoned.store(true, Ordering::Relaxed);
                None
            }
        }
    }

    // ---- Query API (FEATURES.md §4) ----

    pub fn server(&self) -> Option<Server> {
        self.with(|b| b.server.clone()).flatten()
    }

    pub fn channel(&self, id: &ChannelId) -> Option<Channel> {
        self.with(|b| b.channels.get(id).cloned()).flatten()
    }

    pub fn channels(&self) -> Vec<Channel> {
        self.with(|b| b.channels.values().cloned().collect())
        .unwrap_or_default()
    }

    pub fn member(&self, id: &MemberId) -> Option<Member> {
        self.with(|b| b.members.get(id).cloned()).flatten()
    }

    pub fn members(&self) -> Vec<Member> {
        self.with(|b| b.members.values().cloned().collect())
        .unwrap_or_default()
    }

    pub fn member_state(&self, id: &MemberId) -> Option<MemberState> {
        self.with(|b| b.member_states.get(id).cloned()).flatten()
    }

    pub fn roles(&self) -> Vec<Role> {
        self.with(|b| b.roles.values().cloned().collect())
        .unwrap_or_default()
    }

    pub fn voice_state(&self, id: &MemberId) -> Option<VoiceState> {
        self.with(|b| b.voice_states.get(id).cloned()).flatten()
    }

    /// Members currently inside a channel.
    pub fn channel_members(&self, id: &ChannelId) -> Vec<Member> {
        self.with(|b| {
            b.members
                .values()
                .filter(|m| m.channel_id.as_ref() == Some(id))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
    }

    /// Render the channel hierarchy (parents before children, siblings in
    /// `order`).
    pub fn channel_tree(&self) -> Vec<(usize, Channel)> {
        self.with(|b| {
            let mut out = Vec::new();
            let roots: Vec<Channel> = b
                .channels
                .values()
                .filter(|c| c.parent_id.is_none())
                .cloned()
                .collect();
            // Sort roots by (order, id).
            let mut roots = roots;
            roots.sort_by(|a, b| (a.order, a.id.as_str()).cmp(&(b.order, b.id.as_str())));
            fn walk(
                parent: &ChannelId,
                depth: usize,
                b: &BookInner,
                out: &mut Vec<(usize, Channel)>,
            ) {
                let mut children: Vec<Channel> = b
                    .channels
                    .values()
                    .filter(|c| c.parent_id.as_ref() == Some(parent))
                    .cloned()
                    .collect();
                children.sort_by(|a, b| (a.order, a.id.as_str()).cmp(&(b.order, b.id.as_str())));
                for c in children {
                    out.push((depth + 1, c.clone()));
                    walk(&c.id, depth + 1, b, out);
                }
            }
            for r in roots {
                out.push((0, r.clone()));
                walk(&r.id, 0, b, &mut out);
            }
            out
        })
        .unwrap_or_default()
    }

    pub fn find_channel(&self, name: &str) -> Option<Channel> {
        self.with(|b| b.channels.values().find(|c| c.name == name).cloned())
            .flatten()
    }

    pub fn find_member(&self, name: &str) -> Option<Member> {
        self.with(|b| b.members.values().find(|m| m.nickname == name).cloned())
            .flatten()
    }

    /// Clear everything (full rebuild after reconnect, FEATURES.md §2.4).
    pub fn clear(&self) {
        self.with_mut(|b| {
            *b = BookInner::default();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::ChannelId;

    fn channel(id: u64, parent: Option<u64>, name: &str) -> Channel {
        Channel {
            id: ChannelId::from_u64(id),
            parent_id: parent.map(ChannelId::from_u64),
            name: name.into(),
            ..Default::default()
        }
    }

    #[test]
    fn tree_rendering_and_lookup() {
        let book = Book::default();
        book.with_mut(|b| {
            b.channels.insert(1u64.into(), channel(1, None, "Root"));
            b.channels.insert(2u64.into(), channel(2, Some(1), "Sub A"));
            b.channels.insert(3u64.into(), channel(3, Some(1), "Sub B"));
            b.channels.insert(4u64.into(), channel(4, None, "Second Root"));
        });

        let tree = book.channel_tree();
        assert_eq!(tree.len(), 4);
        // Pre-order: parent first, then its children.
        assert_eq!((tree[0].0, tree[0].1.name.as_str()), (0, "Root"));
        assert_eq!((tree[1].0, tree[1].1.name.as_str()), (1, "Sub A"));
        assert_eq!((tree[2].0, tree[2].1.name.as_str()), (1, "Sub B"));
        assert_eq!((tree[3].0, tree[3].1.name.as_str()), (0, "Second Root"));

        assert_eq!(book.find_channel("Sub A").unwrap().id.as_str(), "2");
        assert_eq!(book.channel(&2u64.into()).unwrap().name, "Sub A");
        assert_eq!(book.channels().len(), 4);
    }

    #[test]
    fn disabled_book_returns_none() {
        let book = Book::new(BookConfig {
            enabled: false,
            member_states: false,
        });
        book.with_mut(|b| b.channels.insert(1u64.into(), channel(1, None, "x")));
        assert!(book.channel(&1u64.into()).is_none());
        assert!(book.channels().is_empty());
    }

    #[test]
    fn row_diffing() {
        let mut old = BTreeMap::new();
        old.insert("a".into(), "1".into());
        old.insert("b".into(), "2".into());
        let mut new = BTreeMap::new();
        new.insert("a".into(), "1".into());
        new.insert("b".into(), "3".into());
        new.insert("c".into(), "4".into());
        let changes = diff_rows(&old, &new);
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&PropertyChange::changed("b", "2", "3")));
        assert!(changes.contains(&PropertyChange::added("c", "4")));
    }
}
