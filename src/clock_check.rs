//! The clock check (design "Time"): at startup and every 15 minutes the
//! server measures its offset from `time_server` over SNTP, logs at `error`
//! when the offset is over 2 seconds, and keeps the result for `/healthz`.
//! The check never changes `/healthz`'s status: a time-server outage must not
//! take the server out of service.

use std::time::Duration;

use serde::Serialize;

use crate::clock::{format_timestamp, Clock};
use crate::ntp;

/// How often the check runs after the first one (at startup).
pub const CHECK_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// How long one check may take, DNS included.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

/// The largest offset, in milliseconds, that is not logged as an error.
pub const MAX_OFFSET_MS: i64 = 2_000;

/// The last check, as `/healthz` shows it. Coarse on purpose: the route
/// needs no key and is public, so the server's name and the error text go to
/// the logs only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClockStatus {
    /// Whether the server answered with a usable reply.
    pub ok: bool,
    /// When the check ran (a catalog timestamp, from our clock).
    pub checked_at: String,
    /// The server's clock minus ours, in milliseconds; absent on failure.
    // `skip_serializing_if` leaves the key out of the JSON for `None`, rather
    // than writing `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset_ms: Option<i64>,
    /// The server's stratum; absent on failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stratum: Option<u8>,
}

/// Runs one check and logs its outcome (with the server and the error).
///
/// # Arguments
/// * `server` - the `time_server`.
/// * `clock` - our clock: the check's time, and both ends of the exchange.
/// * `timeout` - the limit for the query ([`QUERY_TIMEOUT`] in the server).
///
/// # Returns
/// The status: an offset and stratum, or `ok: false`. A failure is a status,
/// not an error, so the function has no `Result`.
pub async fn check_once(server: &str, clock: &dyn Clock, timeout: Duration) -> ClockStatus {
    let checked_at = format_timestamp(clock.now());
    match ntp::query(server, clock, timeout).await {
        Ok(reading) => {
            let offset_ms = reading.offset.num_milliseconds();
            if offset_ms.abs() > MAX_OFFSET_MS {
                tracing::error!(server, offset_ms, "the clock is off by more than 2 seconds");
            } else {
                tracing::debug!(
                    server,
                    offset_ms,
                    stratum = reading.stratum,
                    "clock checked"
                );
            }
            ClockStatus {
                ok: true,
                checked_at,
                offset_ms: Some(offset_ms),
                stratum: Some(reading.stratum),
            }
        }
        Err(error) => {
            // `%error` logs the value through its `Display` (the message);
            // `NtpError`'s messages name the server.
            tracing::warn!(%error, "the clock check failed");
            ClockStatus {
                ok: false,
                checked_at,
                offset_ms: None,
                stratum: None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};
    use tokio::net::UdpSocket;

    use super::*;
    use crate::ntp::testing::{fake_server, StepClock};

    fn at(millis: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap() + TimeDelta::milliseconds(millis)
    }

    #[tokio::test]
    async fn a_check_records_the_offset_and_stratum() {
        let server = fake_server(Some((at(5040), at(5060))), 1).await.to_string();
        // The check's own time, then both ends of the exchange.
        let clock = StepClock::new([at(0), at(0), at(100)]);

        let status = check_once(&server, &clock, QUERY_TIMEOUT).await;

        assert_eq!(
            status,
            ClockStatus {
                ok: true,
                checked_at: "2026-11-03T12:00:00Z".to_string(),
                offset_ms: Some(5_000),
                stratum: Some(1),
            }
        );
    }

    #[tokio::test]
    async fn a_failed_check_records_why() {
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server = silent.local_addr().unwrap().to_string();
        let clock = StepClock::new([at(0), at(0), at(1)]);

        let status = check_once(&server, &clock, Duration::from_millis(200)).await;

        assert_eq!(
            (status.ok, status.offset_ms, status.stratum),
            (false, None, None)
        );
    }

    #[test]
    fn a_status_names_no_server_and_no_error() {
        let failed = ClockStatus {
            ok: false,
            checked_at: "2026-11-03T12:00:00Z".to_string(),
            offset_ms: None,
            stratum: None,
        };
        assert_eq!(
            serde_json::to_value(&failed).unwrap(),
            serde_json::json!({"ok": false, "checked_at": "2026-11-03T12:00:00Z"})
        );
    }
}
