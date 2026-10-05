//! Platform-agnostic voice pipeline (FEATURES.md §6): Opus codec wrapper,
//! per-member jitter buffering and mixing of concurrent speakers into one
//! PCM stream. Transport is the platform driver's job.

pub mod codec;
pub mod jitter;
pub mod mixer;
pub mod source;

pub use codec::{OpusDecoder, OpusEncoder};
pub use jitter::JitterBuffer;
pub use mixer::Mixer;
pub use source::SineSource;

/// Canonical voice frame: 20 ms of 48 kHz mono PCM.
pub const FRAME_MS: u32 = 20;
pub const FRAME_SAMPLES: usize = 960;
/// TS3 codec byte for Opus voice — canonical definition lives in
/// `univox_ts3_proto` (usable without libopus); re-exported for compat.
pub use univox_ts3_proto::CODEC_OPUS_VOICE;
