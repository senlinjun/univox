//! Convenience sources for tests and simple bots.

use async_trait::async_trait;


use univox_core::error::Result;

use crate::FRAME_SAMPLES;

/// Generates a sine wave at `freq` Hz (48 kHz mono), frame after frame.
pub struct SineSource {
    freq: f32,
    phase: f32,
}

impl SineSource {
    pub fn new(freq: f32) -> Self {
        Self { freq, phase: 0.0 }
    }

    /// One 20 ms frame of the sine wave.
    pub fn next_frame(&mut self) -> Vec<f32> {
        let step = 2.0 * std::f32::consts::PI * self.freq / 48_000.0;
        let out: Vec<f32> = (0..FRAME_SAMPLES)
            .map(|_| {
                let s = self.phase.sin() * 0.4;
                self.phase += step;
                s
            })
            .collect();
        // Keep the phase bounded to avoid f32 drift over long streams.
        self.phase %= 2.0 * std::f32::consts::PI;
        out
    }
}

#[async_trait::async_trait]
impl univox_core::audio::AudioSource for SineSource {
    async fn read(&mut self, out: &mut [f32]) -> Result<usize> {
        let frame = self.next_frame();
        let n = out.len().min(frame.len());
        out[..n].copy_from_slice(&frame[..n]);
        Ok(n)
    }
}
