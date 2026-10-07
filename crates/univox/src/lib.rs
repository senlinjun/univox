//! # univox
//!
//! Univox 门面（facade）crate：依赖这一个即可获得统一抽象与全部已交付的
//! 平台驱动，不必逐个依赖子 crate。子 crate 仍可单独依赖，并在此整体
//! re-export：
//!
//! - [`core`] —— 统一抽象（原 univox-core）：数据模型、会话生命周期、
//!   事件总线、状态簿记、音频 PCM trait。
//! - [`ts3`] —— TeamSpeak 3 驱动（原 univox-ts3）：原生客户端协议 +
//!   ServerQuery 管理驱动、`Ts3Ext` 平台扩展、文件传输句柄。
//! - [`proto`] —— TS3 线协议类型（原 univox-ts3-proto）：`Command`、
//!   `Row`、`Identity`、`hash_password`，供 `exec` 逃生舱口与身份
//!   持久化使用。
//! - [`voice`] —— 平台无关语音管线（`voice` feature，默认开启）。
//!
//! 常用项在本 crate 根平铺 re-export（两个子 crate 根导出的并集，无重名）：
//!
//! ```
//! use univox::{
//!     ConnectOptions, Credential, Event, Platform, SessionManager, SessionRequest, Ts3Driver,
//! };
//! ```
//!
//! # 快速上手
//!
//! ```rust,no_run
//! use univox::{
//!     ConnectOptions, Credential, Event, Platform, SessionManager, SessionRequest, Ts3Driver,
//! };
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let manager = SessionManager::new();
//!     manager.register_driver(std::sync::Arc::new(Ts3Driver));
//!     let opts = ConnectOptions::new("ts3server://ts.example.com?nickname=MyBot")
//!         .credential(Credential::Anonymous);
//!     let session = manager.connect(SessionRequest::new(Platform::Ts3, opts)).await?;
//!
//!     let mut events = session.events();
//!     while let Some(ev) = events.next().await {
//!         match &*ev {
//!             Event::Connected => println!("ready"),
//!             // 统一事件：消息、成员进出、说话起止……
//!             _ => {}
//!         }
//!     }
//!     Ok(())
//! }
//! ```

// 子 crate 整体 re-export：univox::core::… / univox::ts3::… / univox::proto::…
// 与 univox::voice::…（voice feature）。
pub use univox_core as core;
pub use univox_ts3 as ts3;
pub use univox_ts3_proto as proto;
#[cfg(feature = "voice")]
pub use univox_voice as voice;

// --- 统一抽象（univox-core 根导出全集） ---
pub use univox_core::{
    audio::{AudioCapability, AudioFormat, AudioPacket, AudioSink, AudioSource},
    bookkeeping::{Book, BookConfig, PropertyChange},
    connect::{
        BookkeepingConfig, Capabilities, ConnectOptions, InitialChannel, InitialState,
        NetworkConfig, ReconnectPolicy, SessionRequest,
    },
    credential::{Credential, CredentialProvider, CredentialStore},
    error::{Error, Result},
    event::{CallbackHandle, Category, Event, EventBus, EventFilter, EventStream, RawEvent},
    id::{ChannelId, DbId, MemberId, MessageId, RoleId, ServerId, SessionId},
    message::{Attachment, MessageContent, RichText, Span},
    model::{
        Channel, ChannelKind, ChannelOptions, ConnectionStats, DisconnectReason, HostMessageMode,
        Member, MemberState, Message, MessageTarget, OnlineState, Permanence, Role, SelfMember,
        Server, VoiceState,
    },
    platform::Platform,
    ratelimit::{RateLimiter, RateLimits, TokenBucket},
    session::{Driver, Session, SessionCore, SessionManager, StateCell},
};

// --- TeamSpeak 3 驱动（univox-ts3 根导出全集） ---
pub use univox_ts3::{
    avatar_path, map_proto_err, self_clid, ts3_capabilities, ChannelGroup, ClientDbEntry,
    ClientMatch, FileDownload, FileUpload, HandshakeOptions, QueryConnection, QueryOptions,
    QuerySession, SelfUpdate, ServerGroup, TransferChannel, Ts3ConnectOptions, Ts3Driver, Ts3Ext,
    Ts3Session, UdpConnection, WhisperList, WhisperTarget, WHISPER_MAX_TARGETS,
};
