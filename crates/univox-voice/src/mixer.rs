//! Mixes concurrent decoded member streams into a single PCM stream
//! (FEATURES.md §6.4), with optional 3D positional attenuation/panning
//! (§6.5) — purely local rendering, the server never sees positions.

use univox_core::id::MemberId;

use crate::jitter::MemberJitter;

/// A 3D position (metres, right-handed; TS3 uses the same arbitrary
/// client-local coordinate system for listener and members).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn sub(self, o: Self) -> Self {
        Self::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }

    fn dot(self, o: Self) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }

    fn len(self) -> f32 {
        self.dot(self).sqrt()
    }

    fn normalized(self) -> Self {
        let l = self.len();
        if l > f32::EPSILON {
            Self::new(self.x / l, self.y / l, self.z / l)
        } else {
            Self::new(0.0, 0.0, 1.0)
        }
    }

    fn cross(self, o: Self) -> Self {
        Self::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
}

/// Listener pose for positional audio: position plus orientation
/// (forward and up unit-ish vectors; they need not be exactly unit
/// length — they are normalized internally and orthonogonalized).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Listener {
    pos: Vec3,
    forward: Vec3,
    right: Vec3,
}

/// Distance at which a positioned member starts sounding quieter:
/// gain = 1 / (1 + distance / `REFERENCE_DISTANCE`).
pub const REFERENCE_DISTANCE: f32 = 2.0;

/// Pull-style mixer over per-member jitter buffers.
pub struct Mixer {
    jitter: MemberJitter,
    /// Optional 3D rendering: listener pose + per-member positions.
    listener: Option<Listener>,
    positions: std::collections::HashMap<MemberId, Vec3>,
}

impl Mixer {
    pub fn new(jitter_capacity: usize) -> Self {
        Self {
            jitter: MemberJitter::new(jitter_capacity),
            listener: None,
            positions: std::collections::HashMap::new(),
        }
    }

    pub fn push(&mut self, member: MemberId, seq: u16, frame: Vec<f32>) {
        self.jitter.push(member, seq, frame);
    }

    pub fn remove(&mut self, member: &MemberId) {
        self.jitter.remove(member);
        self.positions.remove(member);
    }

    /// Who has audio buffered right now (for speaking indicators).
    pub fn active(&self) -> Vec<MemberId> {
        self.jitter.active()
    }

    /// Enable positional rendering: place the local listener at `pos`
    /// facing `forward` with `up` orientation. Call with `None` to revert
    /// to plain mixing.
    pub fn set_listener(&mut self, pos: Vec3, forward: Vec3, up: Vec3) {
        let forward = forward.normalized();
        // Orthonormalize: right = forward × up (normalized), so the pan
        // axis stays perpendicular even for sloppy input vectors.
        let right = forward.cross(up).normalized();
        self.listener = Some(Listener { pos, forward, right });
    }

    pub fn clear_listener(&mut self) {
        self.listener = None;
        self.positions.clear();
    }

    /// Place `member` at `pos` (only meaningful once the listener is set).
    pub fn set_member_position(&mut self, member: &MemberId, pos: Vec3) {
        self.positions.insert(member.clone(), pos);
    }

    pub fn clear_member_position(&mut self, member: &MemberId) {
        self.positions.remove(member);
    }

    /// Attenuation/pan gains for `member`, or `None` when it mixes
    /// unpositioned. Returns `(gain, pan)` with `pan` in -1 (hard left)
    /// .. 1 (hard right); the stereo split is
    /// `l = gain·cos((pan+1)·π/4)`, `r = gain·sin((pan+1)·π/4)`.
    fn spatial_gains(&self, member: &MemberId) -> Option<(f32, f32)> {
        let listener = self.listener.as_ref()?;
        let pos = *self.positions.get(member)?;
        let rel = pos.sub(listener.pos);
        let distance = rel.len();
        let gain = 1.0 / (1.0 + distance / REFERENCE_DISTANCE);
        // Pan from the component of the direction along the listener's
        // right axis; directly ahead/behind stays centered.
        let dir = rel.normalized();
        let lateral = dir.dot(listener.right).clamp(-1.0, 1.0);
        Some((gain, lateral))
    }

    /// Mix one output frame from every member with buffered audio.
    /// Returns the members that contributed to this frame. Positional
    /// members are distance-attenuated; the output stays mono.
    pub fn mix_frame(&mut self, frame_samples: usize) -> (Vec<f32>, Vec<MemberId>) {
        let mut out = vec![0.0; frame_samples];
        let mut contributors = Vec::new();
        for member in self.jitter.active() {
            if let Some(frame) = self.jitter.pop(&member) {
                let gain = self
                    .spatial_gains(&member)
                    .map(|(g, _)| g)
                    .unwrap_or(1.0);
                for (o, s) in out.iter_mut().zip(frame) {
                    *o += gain * s;
                }
                contributors.push(member);
            }
        }
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }
        (out, contributors)
    }

    /// Mix one output frame with positional panning: interleaved stereo
    /// (`[L0, R0, L1, R1, …]`, 2×`frame_samples`). Positioned members are
    /// distance-attenuated and panned; unpositioned members (and all
    /// members when no listener is set) play centered at full volume.
    pub fn mix_frame_stereo(&mut self, frame_samples: usize) -> (Vec<f32>, Vec<MemberId>) {
        let mut out = vec![0.0; frame_samples * 2];
        let mut contributors = Vec::new();
        for member in self.jitter.active() {
            if let Some(frame) = self.jitter.pop(&member) {
                let (gain, pan) = self.spatial_gains(&member).unwrap_or((1.0, 0.0));
                let angle = (pan + 1.0) * std::f32::consts::FRAC_PI_4;
                let (lg, rg) = (gain * angle.cos(), gain * angle.sin());
                for (i, s) in frame.into_iter().enumerate() {
                    out[i * 2] += lg * s;
                    out[i * 2 + 1] += rg * s;
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

    #[test]
    fn distance_attenuates_mono() {
        let mut m = Mixer::new(8);
        let far = MemberId::from_u64(3);
        m.set_listener(Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.0, 0.0, -1.0), Vec3::new(0.0, 1.0, 0.0));
        // 18 m away on the forward axis: well past the reference distance.
        m.set_member_position(&far, Vec3::new(0.0, 0.0, -18.0));
        m.push(far.clone(), 1, vec![0.5; 4]);
        let (mixed, contributors) = m.mix_frame(4);
        assert_eq!(contributors, vec![far]);
        let expect = 0.5 / (1.0 + 18.0 / REFERENCE_DISTANCE);
        assert!(
            mixed.iter().all(|s| (*s - expect).abs() < 1e-6),
            "attenuated to ~{expect}, got {:?}",
            mixed[0]
        );
    }

    #[test]
    fn no_listener_means_no_attenuation() {
        let mut m = Mixer::new(8);
        let a = MemberId::from_u64(4);
        m.set_member_position(&a, Vec3::new(50.0, 0.0, 0.0));
        m.push(a, 1, vec![0.25; 4]);
        let (mixed, _) = m.mix_frame(4);
        assert!(mixed.iter().all(|s| (*s - 0.25).abs() < 1e-6));
    }

    #[test]
    fn stereo_pans_right_member_right() {
        let mut m = Mixer::new(8);
        let right_member = MemberId::from_u64(5);
        // Listener faces -z; the member stands to the listener's right (+x).
        m.set_listener(
            Vec3::default(),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        m.set_member_position(&right_member, Vec3::new(2.0, 0.0, 0.0));
        m.push(right_member, 1, vec![0.5; 4]);
        let (mixed, _) = m.mix_frame_stereo(4);
        let l: f32 = mixed.iter().step_by(2).sum();
        let r: f32 = mixed.iter().skip(1).step_by(2).sum();
        assert!(r > l, "member on the right is louder on the right ear (l={l}, r={r})");
    }

    #[test]
    fn ahead_member_is_centered() {
        let mut m = Mixer::new(8);
        let ahead = MemberId::from_u64(6);
        m.set_listener(
            Vec3::default(),
            Vec3::new(0.0, 0.0, -1.0),
            Vec3::new(0.0, 1.0, 0.0),
        );
        m.set_member_position(&ahead, Vec3::new(0.0, 0.0, -2.0));
        m.push(ahead, 1, vec![0.5; 4]);
        let (mixed, _) = m.mix_frame_stereo(4);
        let l: f32 = mixed.iter().step_by(2).sum();
        let r: f32 = mixed.iter().skip(1).step_by(2).sum();
        assert!((l - r).abs() < 1e-4, "ahead member is centered (l={l}, r={r})");
    }
}
