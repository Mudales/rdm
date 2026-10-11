//! Token-bucket bandwidth limiter.
//!
//! Workers call [`TokenBucket::acquire`] after receiving each chunk. While a
//! worker sleeps it stops reading the socket, so TCP flow control slows the
//! sender down: the limit applies to the network, not just to disk writes.
//!
//! The bucket goes into debt instead of making callers queue: a caller that
//! overdraws sleeps until its share is paid back, and later callers see the
//! accumulated debt and sleep longer. That keeps the aggregate rate exact
//! without any per-waiter bookkeeping.

use std::num::NonZeroU64;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::time::Instant;

/// Short bursts above the rate are allowed for this long, so idle time is
/// not "saved up" into a huge spike.
const BURST: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(crate) struct TokenBucket {
    /// Bytes per second; 0 means unlimited.
    rate: AtomicU64,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(rate: Option<NonZeroU64>) -> Self {
        Self {
            rate: AtomicU64::new(rate.map_or(0, NonZeroU64::get)),
            state: Mutex::new(State {
                tokens: 0.0,
                last: Instant::now(),
            }),
        }
    }

    pub fn set_rate(&self, rate: Option<NonZeroU64>) {
        self.rate
            .store(rate.map_or(0, NonZeroU64::get), Ordering::Relaxed);
    }

    pub fn rate(&self) -> Option<NonZeroU64> {
        NonZeroU64::new(self.rate.load(Ordering::Relaxed))
    }

    /// Consumes `bytes` tokens, sleeping if the bucket is in debt.
    pub async fn acquire(&self, bytes: u64) {
        let wait = {
            let rate = self.rate.load(Ordering::Relaxed);
            let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if rate == 0 {
                st.tokens = 0.0;
                st.last = now;
                return;
            }
            let rate = rate as f64;
            let burst = rate * BURST.as_secs_f64();
            let elapsed = now.saturating_duration_since(st.last).as_secs_f64();
            st.tokens = (st.tokens + elapsed * rate).min(burst);
            st.last = now;
            st.tokens -= bytes as f64;
            (st.tokens < 0.0).then(|| Duration::from_secs_f64(-st.tokens / rate))
        };
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn unlimited_never_sleeps() {
        let b = TokenBucket::new(None);
        let t = Instant::now();
        for _ in 0..1000 {
            b.acquire(1 << 20).await;
        }
        assert_eq!(t.elapsed(), Duration::ZERO);
    }

    #[tokio::test(start_paused = true)]
    async fn enforces_rate() {
        let b = TokenBucket::new(NonZeroU64::new(100_000));
        let t = Instant::now();
        for _ in 0..100 {
            b.acquire(10_000).await; // 1 MB total at 100 kB/s
        }
        let secs = t.elapsed().as_secs_f64();
        assert!((9.5..=10.5).contains(&secs), "took {secs}s");
    }

    #[tokio::test(start_paused = true)]
    async fn shared_between_tasks() {
        let b = std::sync::Arc::new(TokenBucket::new(NonZeroU64::new(50_000)));
        let t = Instant::now();
        let tasks: Vec<_> = (0..4)
            .map(|_| {
                let b = b.clone();
                tokio::spawn(async move {
                    for _ in 0..25 {
                        b.acquire(5_000).await;
                    }
                })
            })
            .collect();
        for task in tasks {
            task.await.unwrap();
        }
        // 4 * 25 * 5 kB = 500 kB at 50 kB/s
        let secs = t.elapsed().as_secs_f64();
        assert!((9.5..=10.5).contains(&secs), "took {secs}s");
    }

    #[tokio::test(start_paused = true)]
    async fn rate_can_change() {
        let b = TokenBucket::new(NonZeroU64::new(1_000));
        b.set_rate(None);
        let t = Instant::now();
        b.acquire(1_000_000).await;
        assert_eq!(t.elapsed(), Duration::ZERO);
        assert_eq!(b.rate(), None);
    }
}
