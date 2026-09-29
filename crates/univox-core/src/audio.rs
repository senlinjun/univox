//! Audio abstractions (FEATURES.md §6/§7/G7): the library deals in f32 PCM
//! streams; capture/playback devices remain the application's job.

use crate::error::Result;
use crate::id::MemberId;

/// PCM format contract for the audio pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u8,
}

impl AudioFormat {
    /// The library's canonical format: 48 kHz mono (Opus voice).
    pub const CANONICAL: Self = Self {
        sample_rate: 48_000,
        channels: 1,
    };

    pub fn stereo_48k() -> Self {
        Self {
            sample_rate: 48_000,
            channels: 2,
        }
    }

    pub fn frame_samples(&self, millis: u32) -> usize {
        (self.sample_rate as usize * millis as usize / 1000) * self.channels as usize
    }
}

/// One decoded audio chunk from a member (None = our own send path).
#[derive(Debug, Clone)]
pub struct AudioPacket {
    pub member: Option<MemberId>,
    pub samples: Vec<f32>,
}

/// Pull-style PCM source the application implements (FEATURES.md §6.2).
#[async_trait::async_trait]
pub trait AudioSource: Send {
    /// Fill `out` with interleaved f32 samples in the canonical format.
    /// Returns the number of samples written (0 = silence/end of stream).
    async fn read(&mut self, out: &mut [f32]) -> Result<usize>;
}

/// Push-style PCM sink the application implements (FEATURES.md §6.3).
#[async_trait::async_trait]
pub trait AudioSink: Send {
    /// Consume decoded samples. `member` identifies the speaker; the mixer
    /// calls this once per output frame (mixed) or per member (unmixed).
    async fn write(&mut self, packet: AudioPacket) -> Result<()>;
}

/// Audio capability levels (FEATURES.md §6.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCapability {
    /// TS3: full duplex send+receive.
    FullDuplex,
    /// KOOK public API: push-only (no receiving).
    PushOnly,
    /// OOPZ: bridged through an RTC SDK.
    RtcBridge,
    None,
}

/// Silence helper: fill a buffer with zeros.
pub fn silence(out: &mut [f32]) -> usize {
    out.fill(0.0);
    out.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_sizes() {
        // 20 ms at 48 kHz mono = 960 samples (TS3 voice frame).
        assert_eq!(AudioFormat::CANONICAL.frame_samples(20), 960);
        assert_eq!(AudioFormat::stereo_48k().frame_samples(20), 1920);
    }

    #[tokio::test]
    async fn silence_source() {
        struct ZeroSource;
        #[async_trait::async_trait]
        impl AudioSource for ZeroSource {
            async fn read(&mut self, out: &mut [f32]) -> Result<usize> {
                Ok(silence(out))
            }
        }
        let mut src = ZeroSource;
        let mut buf = vec![1.0f32; 960];
        assert_eq!(src.read(&mut buf).await.unwrap(), 960);
        assert!(buf.iter().all(|&s| s == 0.0));
    }
}
