//! Rate limiting (FEATURES.md §12): a token-bucket default plus the
//! `RateLimiter` abstraction drivers plug platform budgets into.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Rate limiter abstraction. `acquire` waits until budget is available.
#[async_trait::async_trait]
pub trait RateLimiter: Send + Sync {
    async fn acquire(&self, permits: u32);
    fn try_acquire(&self, permits: u32) -> bool;
}

/// Token bucket: `capacity` tokens, refilled at `refill_per_sec`.
#[derive(Debug)]
pub struct TokenBucket {
    state: Mutex<BucketState>,
    capacity: f64,
    refill_per_sec: f64,
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

impl TokenBucket {
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self {
            state: Mutex::new(BucketState {
                tokens: capacity,
                last_refill: Instant::now(),
            }),
            capacity,
            refill_per_sec,
        }
    }

    fn refill_and_take(&self, permits: u32) -> bool {
        let mut state = match self.state.lock() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        let elapsed = now.duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        state.last_refill = now;
        if state.tokens >= permits as f64 {
            state.tokens -= permits as f64;
            true
        } else {
            false
        }
    }
}

#[async_trait::async_trait]
impl RateLimiter for TokenBucket {
    async fn acquire(&self, permits: u32) {
        loop {
            if self.refill_and_take(permits) {
                return;
            }
            let deficit = permits as f64;
            let wait = Duration::from_secs_f64((deficit / self.refill_per_sec).max(0.02));
            tokio::time::sleep(wait.min(Duration::from_secs(2))).await;
        }
    }

    fn try_acquire(&self, permits: u32) -> bool {
        self.refill_and_take(permits)
    }
}

/// Platform default budgets (FEATURES.md §12).
#[derive(Debug, Clone, Copy)]
pub struct RateLimits {
    /// Max commands in `window`.
    pub burst: u32,
    pub window: Duration,
}

impl RateLimits {
    pub fn token_bucket(self) -> TokenBucket {
        TokenBucket::new(
            self.burst as f64,
            self.burst as f64 / self.window.as_secs_f64().max(0.001),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn bucket_burst_and_refill() {
        let limiter = Arc::new(RateLimits {
            burst: 2,
            window: Duration::from_millis(200),
        }
        .token_bucket());
        assert!(limiter.try_acquire(2));
        assert!(!limiter.try_acquire(1));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(limiter.try_acquire(1));
    }

    #[tokio::test]
    async fn acquire_waits() {
        let limiter = Arc::new(RateLimits {
            burst: 1,
            window: Duration::from_millis(100),
        }
        .token_bucket());
        let start = std::time::Instant::now();
        limiter.acquire(1).await;
        limiter.acquire(1).await; // must wait for refill
        assert!(start.elapsed() >= Duration::from_millis(50));
    }
}
