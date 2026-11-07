//! Bounded retry scheduling never retries client authentication failures.

use std::time::Duration;

/// Retry limits shared by provider transports.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Maximum additional attempts.
    pub retries: u32,
    /// Initial exponential delay.
    pub base: Duration,
    /// Maximum backoff delay.
    pub cap: Duration,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            retries: 3,
            base: Duration::from_millis(250),
            cap: Duration::from_secs(30),
        }
    }
}

impl Policy {
    /// Determine retry delay using injectable jitter for deterministic tests.
    pub fn delay(
        &self,
        attempt: u32,
        status: u16,
        retry_after: Option<Duration>,
        jitter: u64,
    ) -> Option<Duration> {
        if attempt >= self.retries || !matches!(status, 408 | 429 | 500 | 502 | 503 | 504 | 529) {
            return None;
        }
        if let Some(delay) = retry_after {
            return Some(delay.min(Duration::from_secs(300)));
        }
        let factor = 1_u32.checked_shl(attempt.min(30)).unwrap_or(u32::MAX);
        Some((self.base.saturating_mul(factor) + Duration::from_millis(jitter % 251)).min(self.cap))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retries_only_transient_failures() {
        let policy = Policy::default();
        assert_eq!(
            policy.delay(0, 429, Some(Duration::from_secs(3)), 0),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            policy.delay(1, 503, None, 0),
            Some(Duration::from_millis(500))
        );
        assert!(policy.delay(0, 401, None, 0).is_none());
        assert!(policy.delay(3, 503, None, 0).is_none());
    }
}
