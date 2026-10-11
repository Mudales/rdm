//! Retry policy with exponential backoff.

use std::time::Duration;

/// Upper bound on a server-requested `Retry-After`, so a misbehaving server
/// cannot park a download for hours.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(300);

/// How transient failures are retried.
///
/// Failures are counted per connection and reset whenever that connection
/// makes progress, so a long download over a flaky link is not killed by
/// the total number of hiccups, only by `max_retries` failures in a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Consecutive failures tolerated before giving up.
    pub max_retries: u32,
    /// Delay before the first retry.
    pub base_delay: Duration,
    /// Cap on the exponential delay.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: 8,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl RetryPolicy {
    /// No retries at all.
    pub fn none() -> Self {
        Self {
            max_retries: 0,
            ..Self::default()
        }
    }

    /// Delay before retry number `attempt` (1-based).
    ///
    /// Uses "equal jitter": half the exponential delay is fixed, half random,
    /// so many connections failing together do not retry in lockstep. A
    /// server's `Retry-After` is honored when it asks for longer.
    pub fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        let exp = 2u32.saturating_pow(attempt.saturating_sub(1).min(31));
        let backoff = self.base_delay.saturating_mul(exp).min(self.max_delay);
        let half = backoff / 2;
        let jittered = half + half.mul_f64(fastrand::f64());
        match retry_after {
            Some(ra) => ra.min(MAX_RETRY_AFTER).max(jittered),
            None => jittered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grows_exponentially_and_caps() {
        let p = RetryPolicy {
            max_retries: 10,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(10),
        };
        for (attempt, full) in [(1, 1), (2, 2), (3, 4), (4, 8), (5, 10), (30, 10), (99, 10)] {
            let d = p.delay(attempt, None);
            let full = Duration::from_secs(full);
            assert!(d >= full / 2 && d <= full, "attempt {attempt}: {d:?}");
        }
    }

    #[test]
    fn honors_retry_after() {
        let p = RetryPolicy::default();
        assert_eq!(
            p.delay(1, Some(Duration::from_secs(20))),
            Duration::from_secs(20)
        );
        assert_eq!(p.delay(1, Some(Duration::from_secs(9999))), MAX_RETRY_AFTER);
    }
}
