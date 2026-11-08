//! Bounded retry scheduling never retries client authentication failures.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Parse delay seconds or an IMF-fixdate without accepting unbounded sleeps.
/// The transport's overall deadline remains authoritative for server delays.
pub fn retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<u64>() {
        return Some(Duration::from_secs(seconds.min(300)));
    }
    let fields: Vec<_> = value.split_ascii_whitespace().collect();
    if fields.len() != 6 || !fields[0].ends_with(',') || fields[5] != "GMT" {
        return None;
    }
    let day = fields[1].parse::<i64>().ok()?;
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|month| *month == fields[2])? as i64
        + 1;
    let year = fields[3].parse::<i64>().ok()?;
    let clock: Vec<_> = fields[4].split(':').collect();
    if clock.len() != 3 || !(1970..=9999).contains(&year) {
        return None;
    }
    let hour = clock[0].parse::<u64>().ok()?;
    let minute = clock[1].parse::<u64>().ok()?;
    let second = clock[2].parse::<u64>().ok()?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if !(1..=days).contains(&day) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    // Shift January/February to the preceding year for Gregorian era arithmetic.
    let year = year - i64::from(month <= 2);
    let era = year / 400;
    let yoe = year - era * 400;
    let shifted_month = month + if month > 2 { -3 } else { 9 };
    let doy = (153 * shifted_month + 2) / 5 + day - 1;
    let days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let seconds = u64::try_from(days).ok()? * 86400 + hour * 3600 + minute * 60 + second;
    let date = UNIX_EPOCH.checked_add(Duration::from_secs(seconds))?;
    Some(
        date.duration_since(now)
            .unwrap_or_default()
            .min(Duration::from_secs(300)),
    )
}

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
        Some(
            self.base
                .saturating_mul(factor)
                .saturating_add(Duration::from_millis(jitter % 251))
                .min(self.cap),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_bounded_retry_after_dates_and_seconds() {
        let now = UNIX_EPOCH + Duration::from_secs(784111770);
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:37 GMT", now),
            Some(Duration::from_secs(7))
        );
        assert_eq!(
            retry_after("Sun, 06 Nov 1994 08:49:00 GMT", now),
            Some(Duration::ZERO)
        );
        assert_eq!(
            retry_after("999999999", now),
            Some(Duration::from_secs(300))
        );
        assert_eq!(retry_after("Sun, 31 Feb 2024 08:49:37 GMT", now), None);
        assert_eq!(retry_after("-1", now), None);
        assert_eq!(retry_after("invalid", now), None);
    }

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
