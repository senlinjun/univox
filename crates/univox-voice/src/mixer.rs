//! Mixes concurrent decoded member streams into a single PCM stream
//! (FEATURES.md §6.4).

use univox_core::id::MemberId;

use crate::jitter::MemberJitter;

/// Pull-style mixer over per-member jitter buffers.
pub struct Mixer {
    jitter: MemberJitter,
}

impl Mixer {
    pub fn new(jitter_capacity: usize) -> Self {
        Self {
            jitter: MemberJitter::new(jitter_capacity),
        }
    }

    pub fn push(&mut self, member: MemberId, seq: u16, frame: Vec<f32>) {
        self.jitter.push(member, seq, frame);
    }

    pub fn remove(&mut self, member: &MemberId) {
        self.jitter.remove(member);
    }

    /// Who has audio buffered right now (for speaking indicators).
    pub fn active(&self) -> Vec<MemberId> {
        self.jitter.active()
    }

    /// Mix one output frame from every member with buffered audio.
    /// Returns the members that contributed to this frame.
    pub fn mix_frame(&mut self, frame_samples: usize) -> (Vec<f32>, Vec<MemberId>) {
        let mut out = vec![0.0; frame_samples];
        let mut contributors = Vec::new();
        for member in self.jitter.active() {
            if let Some(frame) = self.jitter.pop(&member) {
                for (o, s) in out.iter_mut().zip(frame) {
                    *o += s;
                }
                contributors.push(member);
            }
        }
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }
        (out, contributors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixes_members() {
        let mut m = Mixer::new(8);
        let a = MemberId::from_u64(1);
        let b = MemberId::from_u64(2);
        let f: Vec<f32> = vec![0.5; 4];
        m.push(a, 1, f.clone());
        m.push(b, 1, f.clone());
        let (mixed, contributors) = m.mix_frame(4);
        assert_eq!(contributors.len(), 2);
        assert!(mixed.iter().all(|s| (*s - 1.0).abs() < 1e-6), "0.5+0.5 mixed");
        let (empty, none) = m.mix_frame(4);
        assert!(none.is_empty() && empty.iter().all(|s| *s == 0.0));
    }
}
