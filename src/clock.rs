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

/// `now` plus a configured duration, checked: a duration too long for a
/// timestamp is refused instead of panicking or wrapping.
///
/// # Arguments
/// * `now` - the start (server clock).
/// * `duration` - how long after it (a std `Duration`, as config and
///   settings hold them).
///
/// # Returns
/// The instant `duration` after `now`.
///
/// # Errors
/// [`crate::error::OkfError::LimitExceeded`] with `limit: "duration"` when
/// the sum is past chrono's range.
pub fn after(
    now: DateTime<Utc>,
    duration: std::time::Duration,
) -> Result<DateTime<Utc>, crate::error::OkfError> {
    // `from_std` fails for durations beyond chrono's range; `checked_add_signed`
    // for sums beyond the calendar's. `and_then` chains the two `Option`s.
    chrono::TimeDelta::from_std(duration)
        .ok()
        .and_then(|delta| now.checked_add_signed(delta))
        .ok_or_else(|| crate::error::OkfError::LimitExceeded {
            limit: "duration",
            reason: format!("{duration:?} is too long"),
        })
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
    fn after_adds_a_std_duration_and_refuses_an_absurd_one() {
        let start = Utc.with_ymd_and_hms(2026, 11, 12, 12, 0, 0).unwrap();

        let later = after(start, std::time::Duration::from_secs(90)).unwrap();
        let absurd = after(start, std::time::Duration::from_secs(u64::MAX));

        assert_eq!(
            later,
            Utc.with_ymd_and_hms(2026, 11, 12, 12, 1, 30).unwrap()
        );
        assert!(matches!(
            absurd,
            Err(crate::error::OkfError::LimitExceeded {
                limit: "duration",
                ..
            })
        ));
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
