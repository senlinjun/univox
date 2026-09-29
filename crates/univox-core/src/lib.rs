//! # univox-core
//!
//! Unified abstractions for voice platforms (FEATURES.md §1–§12):
//! a platform-agnostic data model, session lifecycle, event bus,
//! state bookkeeping, audio PCM traits and cross-cutting infrastructure.
//! Platform drivers ([`univox-ts3`], future KOOK/OOPZ) implement
//! [`Session`] and [`Driver`] on top of these.

pub mod audio;
pub mod bookkeeping;
pub mod connect;
pub mod credential;
pub mod error;
pub mod event;
pub mod id;
pub mod message;
pub mod model;
pub mod platform;
pub mod ratelimit;
pub mod session;
pub mod time;

pub use audio::{AudioCapability, AudioFormat, AudioPacket, AudioSink, AudioSource};
pub use bookkeeping::{Book, BookConfig, PropertyChange};
pub use connect::{
    BookkeepingConfig, Capabilities, ConnectOptions, InitialChannel, InitialState, NetworkConfig,
    ReconnectPolicy, SessionRequest,
};
pub use credential::{Credential, CredentialProvider, CredentialStore};
pub use error::{Error, Result};
pub use event::{CallbackHandle, Category, Event, EventBus, EventFilter, EventStream, RawEvent};
pub use id::{
    ChannelId, DbId, MemberId, MessageId, RoleId, ServerId, SessionId,
};
pub use message::{Attachment, MessageContent, RichText, Span};
pub use model::{
    Channel, ChannelKind, ChannelOptions, ConnectionStats, DisconnectReason, HostMessageMode,
    Member, MemberState, Message, MessageTarget, OnlineState, Permanence, Role, SelfMember,
    Server, VoiceState,
};
pub use ratelimit::{RateLimits, RateLimiter, TokenBucket};
pub use session::{Driver, Session, SessionCore, SessionManager, StateCell};

/// Convenience prelude: `use univox_core::prelude::*;`
pub mod prelude {
    pub use crate::audio::*;
    pub use crate::bookkeeping::Book;
    pub use crate::connect::*;
    pub use crate::credential::Credential;
    pub use crate::error::{Error, Result};
    pub use crate::event::*;
    pub use crate::id::*;
    pub use crate::message::MessageContent;
    pub use crate::model::*;
    pub use crate::platform::Platform;
    pub use crate::ratelimit::*;
    pub use crate::session::*;
}
