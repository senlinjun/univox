//! Thin Opus wrapper over `audiopus` for the canonical PCM format
//! (48 kHz mono f32, 20 ms frames).

use std::sync::Mutex;

use univox_core::error::{Error, Result};

use crate::FRAME_SAMPLES;

/// Opus encoder for the canonical format (VOIP application).
pub struct OpusEncoder {
    inner: Mutex<audiopus::coder::Encoder>,
    buf: Mutex<Vec<u8>>,
}

impl OpusEncoder {
    pub fn new() -> Result<Self> {
        let mut inner = audiopus::coder::Encoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
            audiopus::Application::Voip,
        )
        .map_err(opus_err)?;
        // Reasonable default for voice: 32 kbit/s mono.
        inner
            .set_bitrate(audiopus::Bitrate::BitsPerSecond(32_000))
            .map_err(opus_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
            buf: Mutex::new(vec![0u8; 512]),
        })
    }

    /// Encode one 20 ms frame (960 f32 samples) into an Opus packet.
    pub fn encode(&self, samples: &[f32]) -> Result<Vec<u8>> {
        if samples.len() != FRAME_SAMPLES {
            return Err(Error::InvalidArgument(format!(
                "opus frame must be {FRAME_SAMPLES} samples, got {}",
                samples.len()
            )));
        }
        let enc = self.inner.lock().unwrap();
        let mut buf = self.buf.lock().unwrap();
        let n = enc.encode_float(samples, &mut buf[..]).map_err(opus_err)?;
        Ok(buf[..n].to_vec())
    }
}

/// Opus decoder with packet-loss concealment for the canonical format.
pub struct OpusDecoder {
    inner: Mutex<audiopus::coder::Decoder>,
    buf: Mutex<Vec<f32>>,
}

impl OpusDecoder {
    pub fn new() -> Result<Self> {
        let inner = audiopus::coder::Decoder::new(
            audiopus::SampleRate::Hz48000,
            audiopus::Channels::Mono,
        )
        .map_err(opus_err)?;
        Ok(Self {
            inner: Mutex::new(inner),
            buf: Mutex::new(vec![0.0; FRAME_SAMPLES]),
        })
    }

    /// Decode one packet; `None` conceals a lost frame (still returns PCM).
    pub fn decode(&self, packet: Option<&[u8]>) -> Result<Vec<f32>> {
        let dec = self.inner.lock().unwrap();
        let mut dec = dec;
        let mut buf = self.buf.lock().unwrap();
        let packet: Option<audiopus::packet::Packet> = packet
            .map(|p| audiopus::packet::Packet::try_from(p))
            .transpose()
            .map_err(opus_err)?;
        let n = dec
            .decode_float(
                packet,
                audiopus::MutSignals::try_from(&mut buf[..]).map_err(opus_err)?,
                false,
            )
            .map_err(opus_err)?;
        Ok(buf[..n].to_vec())
    }
}

fn opus_err(e: audiopus::Error) -> Error {
    Error::Other(format!("opus: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_sine() {
        let enc = OpusEncoder::new().unwrap();
        let dec = OpusDecoder::new().unwrap();
        let frame: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|i| (i as f32 * 0.05).sin() * 0.5)
            .collect();
        let packet = enc.encode(&frame).unwrap();
        assert!(!packet.is_empty());
        let decoded = dec.decode(Some(&packet)).unwrap();
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        // Energy survives the roundtrip.
        let rms = (decoded.iter().map(|s| s * s).sum::<f32>() / decoded.len() as f32).sqrt();
        assert!(rms > 0.2, "decoded rms too low: {rms}");
    }

    #[test]
    fn concealment_returns_silence() {
        let dec = OpusDecoder::new().unwrap();
        let out = dec.decode(None).unwrap();
        assert_eq!(out.len(), FRAME_SAMPLES);
    }
}
