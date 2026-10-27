//! The source of "now". Every timestamp the server writes comes from here, so
//! tests can inject a fixed instant and agents can never pass one in.

use chrono::{DateTime, Utc};

/// The format of every timestamp written to a catalog: `2026-10-26T23:01:11Z`.
pub const TIMESTAMP_FORMAT: &str = "%Y-%m-%dT%H:%M:%SZ";

/// A source of the current UTC time.
///
/// `Send + Sync` lets one clock be shared across threads and async tasks (Python has no
/// equivalent; Rust checks thread-safety at compile time).
pub trait Clock: Send + Sync {
    /// Returns the current instant in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The real system clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock frozen at one instant, for tests and parity runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock(
    /// The instant every call to `now()` returns.
    pub DateTime<Utc>,
);

impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// Formats an instant as a catalog timestamp (second precision, `Z` suffix).
///
/// # Arguments
/// * `instant` - the UTC instant to format.
///
/// # Returns
/// The timestamp string, e.g. `2026-10-26T23:01:11Z`.
///
/// # Examples
/// ```
/// use chrono::{TimeZone, Utc};
/// use okf_mcp::clock::format_timestamp;
/// let t = Utc.with_ymd_and_hms(2026, 10, 26, 23, 1, 11).unwrap();
/// assert_eq!(format_timestamp(t), "2026-10-26T23:01:11Z");
/// ```
pub fn format_timestamp(instant: DateTime<Utc>) -> String {
    instant.format(TIMESTAMP_FORMAT).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn fixed_clock_returns_its_instant() {
        let instant = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        assert_eq!(FixedClock(instant).now(), instant);
    }

    #[test]
    fn format_drops_subseconds() {
        let instant = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
            + chrono::Duration::milliseconds(999);
        assert_eq!(format_timestamp(instant), "2026-01-02T03:04:05Z");
    }

    #[test]
    fn system_clock_is_close_to_utc_now() {
        let delta = (SystemClock.now() - Utc::now()).num_seconds().abs();
        assert!(delta < 5, "SystemClock drifted {delta}s from Utc::now()");
    }
}
