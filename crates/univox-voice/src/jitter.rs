//! Per-member jitter buffer: orders packets that arrive out of order and
//! absorbs network delay up to `capacity` frames (FEATURES.md §6.4).

use std::collections::HashMap;
use std::collections::VecDeque;

use univox_core::id::MemberId;

/// One member's bounded, sequence-ordered frame queue.
pub struct JitterBuffer {
    queue: VecDeque<(u16, Vec<f32>)>,
    capacity: usize,
    /// Highest sequence number seen (wrapping-aware next expectation).
    next_seq: u16,
    primed: bool,
}

impl JitterBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity,
            next_seq: 0,
            primed: false,
        }
    }

    /// Push a decoded frame with its voice sequence number.
    /// Returns false when the buffer is full (frame dropped).
    pub fn push(&mut self, seq: u16, frame: Vec<f32>) -> bool {
        if self.queue.len() >= self.capacity {
            return false;
        }
        if !self.primed {
            self.next_seq = seq;
            self.primed = true;
        }
        // Drop duplicates / very late frames.
        if seq.wrapping_sub(self.next_seq) > 0x8000 {
            return true;
        }
        // First element strictly AFTER `seq` in wrapping order.
        let pos = self
            .queue
            .iter()
            .position(|(s, _)| seq.wrapping_sub(*s) > 0x8000)
            .unwrap_or(self.queue.len());
        self.queue.insert(pos, (seq, frame));
        true
    }

    /// Pop the next in-order frame, if one is buffered.
    pub fn pop(&mut self) -> Option<Vec<f32>> {
        let (seq, frame) = self.queue.pop_front()?;
        self.next_seq = seq.wrapping_add(1);
        Some(frame)
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }
}

/// Jitter buffers for every currently-speaking member.
#[derive(Default)]
pub struct MemberJitter {
    members: HashMap<MemberId, JitterBuffer>,
    capacity: usize,
}

impl MemberJitter {
    pub fn new(capacity: usize) -> Self {
        Self {
            members: HashMap::new(),
            capacity,
        }
    }

    pub fn push(&mut self, member: MemberId, seq: u16, frame: Vec<f32>) -> bool {
        self.members
            .entry(member)
            .or_insert_with(|| JitterBuffer::new(self.capacity))
            .push(seq, frame)
    }

    pub fn pop(&mut self, member: &MemberId) -> Option<Vec<f32>> {
        self.members.get_mut(member).and_then(|j| j.pop())
    }

    /// Members with buffered frames right now.
    pub fn active(&self) -> Vec<MemberId> {
        self.members
            .iter()
            .filter(|(_, j)| !j.is_empty())
            .map(|(m, _)| m.clone())
            .collect()
    }

    pub fn remove(&mut self, member: &MemberId) {
        self.members.remove(member);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(n: u16) -> Vec<f32> {
        vec![n as f32]
    }

    #[test]
    fn orders_and_drops_late() {
        let mut j = JitterBuffer::new(8);
        assert!(j.push(10, frame(10)));
        assert!(j.push(12, frame(12)));
        assert!(j.push(11, frame(11)));
        assert_eq!(j.pop().unwrap()[0], 10.0);
        assert_eq!(j.pop().unwrap()[0], 11.0);
        assert_eq!(j.pop().unwrap()[0], 12.0);
        assert!(j.pop().is_none());
    }

    #[test]
    fn bounded() {
        let mut j = JitterBuffer::new(2);
        assert!(j.push(1, frame(1)));
        assert!(j.push(2, frame(2)));
        assert!(!j.push(3, frame(3)), "capacity must bound the queue");
    }
}
