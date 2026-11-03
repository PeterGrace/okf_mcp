//! A minimal SNTP client (RFC 4330, NTP version 4): one request, one reply,
//! the clock offset. The server's clock check (design "Time", `clock_check`)
//! runs it against `time_server` at startup and every 15 minutes.
//!
//! No crate: the protocol is one 48-byte packet each way. Timestamps are
//! 64-bit NTP fixed point: seconds since 1900 in the high 32 bits, a binary
//! fraction of a second in the low 32. Seconds wrap in February 2036 (era 1);
//! `from_ntp` reads a timestamp whose top bit is clear as era 1, as RFC 4330
//! section 3 says.

use std::net::SocketAddr;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use tokio::net::{lookup_host, UdpSocket};

use crate::clock::Clock;

/// The NTP port, used when `time_server` names none.
pub const DEFAULT_PORT: u16 = 123;

/// Seconds from the NTP epoch (1900-01-01) to the Unix epoch (1970-01-01).
const NTP_UNIX_OFFSET: i64 = 2_208_988_800;

/// An NTP packet without extension fields.
const PACKET_LEN: usize = 48;

/// One measurement against a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Reading {
    /// The server's stratum (1 for a reference clock such as GPS).
    pub stratum: u8,
    /// How far the server's clock is ahead of ours (negative: behind).
    pub offset: TimeDelta,
    /// The network round trip, without the server's own processing time.
    pub round_trip: TimeDelta,
}

/// Why a query gave no reading.
#[derive(Debug, thiserror::Error)]
pub enum NtpError {
    /// The name did not resolve.
    #[error("resolve {server}: {source}")]
    Resolve {
        /// The `time_server` value.
        server: String,
        /// The resolver's error.
        #[source]
        source: std::io::Error,
    },
    /// The name resolved to no address.
    #[error("{server} resolves to no address")]
    NoAddress {
        /// The `time_server` value.
        server: String,
    },
    /// The socket failed.
    #[error("query {server}: {source}")]
    Io {
        /// The `time_server` value.
        server: String,
        /// The socket's error.
        #[source]
        source: std::io::Error,
    },
    /// No reply in time (DNS included).
    #[error("{server} did not answer within {timeout:?}")]
    Timeout {
        /// The `time_server` value.
        server: String,
        /// The limit that passed.
        timeout: Duration,
    },
    /// A reply that can't be used.
    #[error("{server} sent an unusable reply: {reason}")]
    BadReply {
        /// The `time_server` value.
        server: String,
        /// What was wrong with it.
        reason: &'static str,
    },
}

/// The fields of a reply the offset needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Reply {
    stratum: u8,
    receive: u64,
    transmit: u64,
}

/// Measures the clock's offset from `server`.
///
/// # Arguments
/// * `server` - `host`, `host:port`, `a.b.c.d`, `[v6]:port` or a bare IPv6 address.
/// * `clock` - our clock; read just before sending and just after receiving.
/// * `timeout` - the limit for the whole exchange, DNS included.
///
/// When the name resolves to several addresses (A and AAAA records), each is
/// tried in the resolver's order until one gives a usable reply, all within
/// `timeout`. A family with no route fails at once and the next is tried; a
/// silent address uses up the rest of the time.
///
/// # Returns
/// The offset, the round trip and the server's stratum.
///
/// # Errors
/// [`NtpError`]: the name doesn't resolve, no reply in time, or, when every
/// address failed, the last address's error (a socket failure, or a reply
/// that isn't a synchronized server's answer to this request).
pub async fn query(
    server: &str,
    clock: &dyn Clock,
    timeout: Duration,
) -> Result<Reading, NtpError> {
    // `timeout` gives `Err(Elapsed)` if the deadline passes first; `?` on the
    // `map_err` result, then the inner `Result` is returned as it is.
    tokio::time::timeout(timeout, exchange(server, clock))
        .await
        .map_err(|_| NtpError::Timeout {
            server: server.to_string(),
            timeout,
        })?
}

/// Resolves `server` and asks its addresses, without a deadline (see [`query`]).
async fn exchange(server: &str, clock: &dyn Clock) -> Result<Reading, NtpError> {
    // A named binding: the returned iterator borrows the string, so a
    // temporary (freed at the end of the statement) would not live long enough.
    let target = with_port(server);
    let addresses = lookup_host(target.as_str())
        .await
        .map_err(|source| NtpError::Resolve {
            server: server.to_string(),
            source,
        })?;
    ask_each(server, addresses, clock).await
}

/// Asks each address in turn; the first usable reply wins.
///
/// # Errors
/// [`NtpError::NoAddress`] for no addresses, else the last address's error.
async fn ask_each(
    server: &str,
    addresses: impl IntoIterator<Item = SocketAddr>,
    clock: &dyn Clock,
) -> Result<Reading, NtpError> {
    let mut last_error = NtpError::NoAddress {
        server: server.to_string(),
    };
    for address in addresses {
        match ask(server, address, clock).await {
            Ok(reading) => return Ok(reading),
            Err(error) => last_error = error,
        }
    }
    Err(last_error)
}

/// One request to `address` and its reply.
async fn ask(server: &str, address: SocketAddr, clock: &dyn Clock) -> Result<Reading, NtpError> {
    // Closures that wrap an error with the server's name. They borrow
    // `server` (a `&str`, which is `Copy`), so they can be used repeatedly.
    let io = |source: std::io::Error| NtpError::Io {
        server: server.to_string(),
        source,
    };
    let bad = |reason: &'static str| NtpError::BadReply {
        server: server.to_string(),
        reason,
    };
    // Bind the family the server's address has, on any port.
    let local = if address.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from(([0u16; 8], 0))
    };
    let socket = UdpSocket::bind(local).await.map_err(io)?;
    // A connected UDP socket only receives datagrams from that address.
    socket.connect(address).await.map_err(io)?;
    let sent_at = clock.now();
    let sent = to_ntp(sent_at);
    socket.send(&request(sent)).await.map_err(io)?;
    let mut buffer = [0u8; 512];
    let length = socket.recv(&mut buffer).await.map_err(io)?;
    let received_at = clock.now();
    let reply = parse_reply(&buffer[..length], sent).map_err(bad)?;
    let receive = from_ntp(reply.receive).ok_or_else(|| bad("its receive time is out of range"))?;
    let transmit =
        from_ntp(reply.transmit).ok_or_else(|| bad("its transmit time is out of range"))?;
    let (offset, round_trip) = offset_and_round_trip(sent_at, receive, transmit, received_at);
    Ok(Reading {
        stratum: reply.stratum,
        offset,
        round_trip,
    })
}

/// `server` with the NTP port when it names none.
fn with_port(server: &str) -> String {
    if server.parse::<SocketAddr>().is_ok() {
        return server.to_string();
    }
    if server.parse::<std::net::Ipv6Addr>().is_ok() {
        return format!("[{server}]:{DEFAULT_PORT}");
    }
    match server.rsplit_once(':') {
        Some((_, port)) if port.parse::<u16>().is_ok() => server.to_string(),
        _ => format!("{server}:{DEFAULT_PORT}"),
    }
}

/// A client request: leap 0, version 4, mode 3, our transmit time.
fn request(transmit: u64) -> [u8; PACKET_LEN] {
    let mut packet = [0u8; PACKET_LEN];
    // Two bits of leap indicator, three of version, three of mode.
    packet[0] = 0b00_100_011;
    packet[40..48].copy_from_slice(&transmit.to_be_bytes());
    packet
}

/// Checks a reply to the request that carried `sent`.
fn parse_reply(packet: &[u8], sent: u64) -> Result<Reply, &'static str> {
    if packet.len() < PACKET_LEN {
        return Err("shorter than 48 bytes");
    }
    let leap = packet[0] >> 6;
    let mode = packet[0] & 0b111;
    let stratum = packet[1];
    if mode != 4 {
        return Err("not a server reply (mode 4)");
    }
    if leap == 3 {
        return Err("the server is not synchronized (leap indicator 3)");
    }
    if stratum == 0 {
        return Err("a kiss-o'-death reply (stratum 0)");
    }
    if stratum > 15 {
        return Err("the server is not synchronized (stratum above 15)");
    }
    // `try_into` turns the 8-byte slice into an array; the length is fixed.
    let word =
        |at: usize| u64::from_be_bytes(packet[at..at + 8].try_into().expect("an 8-byte field"));
    if word(24) != sent {
        return Err("its originate time is not this request's");
    }
    let receive = word(32);
    if receive == 0 {
        return Err("it has no receive time");
    }
    let transmit = word(40);
    if transmit == 0 {
        return Err("it has no transmit time");
    }
    Ok(Reply {
        stratum,
        receive,
        transmit,
    })
}

/// RFC 4330 section 5: the offset and the round trip from the four times.
fn offset_and_round_trip(
    sent: DateTime<Utc>,
    received_by_server: DateTime<Utc>,
    sent_by_server: DateTime<Utc>,
    received: DateTime<Utc>,
) -> (TimeDelta, TimeDelta) {
    let offset = ((received_by_server - sent) + (sent_by_server - received)) / 2;
    let round_trip = (received - sent) - (sent_by_server - received_by_server);
    (offset, round_trip)
}

/// An instant as an NTP timestamp.
fn to_ntp(instant: DateTime<Utc>) -> u64 {
    // `as u32` keeps the low 32 bits: NTP seconds wrap in 2036, and
    // `from_ntp` undoes the wrap.
    let seconds = (instant.timestamp() + NTP_UNIX_OFFSET) as u32;
    // Rounded to the nearest 2^-32 s, not down: with rounding both ways a
    // nanosecond survives the round trip exactly (the fraction is finer).
    // The largest result, for 999_999_999 ns, is still below 2^32. chrono
    // gives 10^9 ns or more inside a leap second; the clamp keeps those in
    // the fraction instead of letting them spill into the seconds bits.
    let nanos = instant.timestamp_subsec_nanos().min(999_999_999);
    let fraction = ((u64::from(nanos) << 32) + 500_000_000) / 1_000_000_000;
    (u64::from(seconds) << 32) | fraction
}

/// An NTP timestamp as an instant (`None` if chrono can't hold it).
fn from_ntp(stamp: u64) -> Option<DateTime<Utc>> {
    let mut seconds = (stamp >> 32) as i64;
    // RFC 4330 section 3: a clear top bit means era 1 (2036 to 2104).
    if seconds & 0x8000_0000 == 0 {
        seconds += 1 << 32;
    }
    // Rounded to the nearest nanosecond (`+ 2^31` before the shift). A
    // fraction within half a nanosecond of the next second (a server may
    // send one) rounds to 10^9, which carries into the seconds.
    let nanos = ((stamp & 0xFFFF_FFFF) * 1_000_000_000 + (1 << 31)) >> 32;
    let (seconds, nanos) = if nanos == 1_000_000_000 {
        (seconds + 1, 0)
    } else {
        (seconds, nanos)
    };
    Utc.timestamp_opt(seconds - NTP_UNIX_OFFSET, nanos as u32)
        .single()
}

/// Helpers for this module's tests and `clock_check`'s.
#[cfg(test)]
pub(crate) mod testing {
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use chrono::{DateTime, Utc};
    use tokio::net::UdpSocket;

    use super::{to_ntp, PACKET_LEN};
    use crate::clock::Clock;

    /// A clock that returns the given instants in order, one per call.
    pub(crate) struct StepClock(Mutex<VecDeque<DateTime<Utc>>>);

    impl StepClock {
        /// The clock, ready to return `instants` in order.
        pub(crate) fn new(instants: impl IntoIterator<Item = DateTime<Utc>>) -> Self {
            Self(Mutex::new(instants.into_iter().collect()))
        }
    }

    impl Clock for StepClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
                .lock()
                .expect("step clock lock")
                .pop_front()
                .expect("the step clock ran out of instants")
        }
    }

    /// A server that answers one request on `127.0.0.1`.
    ///
    /// # Arguments
    /// * `times` - its (receive, transmit) times; `None` stamps both with
    ///   the system clock when the request arrives, like a real server.
    /// * `stratum` - the stratum it reports.
    ///
    /// # Returns
    /// Its address, for `query`.
    pub(crate) async fn fake_server(
        times: Option<(DateTime<Utc>, DateTime<Utc>)>,
        stratum: u8,
    ) -> SocketAddr {
        let socket = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind a fake NTP server");
        let address = socket.local_addr().expect("its address");
        tokio::spawn(async move {
            let mut request = [0u8; PACKET_LEN];
            if let Ok((_, client)) = socket.recv_from(&mut request).await {
                // `unwrap_or_else` reads the clock only for `None`.
                let (receive, transmit) = times.unwrap_or_else(|| {
                    let now = Utc::now();
                    (now, now)
                });
                let mut reply = [0u8; PACKET_LEN];
                reply[0] = 0b00_100_100; // leap 0, version 4, mode 4 (server)
                reply[1] = stratum;
                reply[24..32].copy_from_slice(&request[40..48]);
                reply[32..40].copy_from_slice(&to_ntp(receive).to_be_bytes());
                reply[40..48].copy_from_slice(&to_ntp(transmit).to_be_bytes());
                // A failed send shows up as the client's timeout.
                let _ = socket.send_to(&reply, client).await;
            }
        });
        address
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{fake_server, StepClock};
    use super::*;

    fn at(millis: i64) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap() + TimeDelta::milliseconds(millis)
    }

    #[test]
    fn timestamps_round_trip_on_both_sides_of_2036() {
        for instant in [
            at(123),
            at(5040),
            at(999) + TimeDelta::nanoseconds(999_999),
            Utc.with_ymd_and_hms(2040, 1, 1, 0, 0, 0).unwrap() + TimeDelta::microseconds(5),
        ] {
            // Both conversions round to nearest, and 2^-32 s is finer than a
            // nanosecond, so the round trip is exact.
            assert_eq!(from_ntp(to_ntp(instant)), Some(instant));
        }
    }

    #[test]
    fn a_fraction_just_under_a_second_carries_into_the_seconds() {
        let base = to_ntp(at(0)) & !0xFFFF_FFFF;
        assert_eq!(from_ntp(base | 0xFFFF_FFFF), Some(at(1_000)));
    }

    #[test]
    fn a_leap_second_instant_stays_in_its_second() {
        let leap = chrono::NaiveDate::from_ymd_opt(2016, 12, 31)
            .and_then(|day| day.and_hms_nano_opt(23, 59, 59, 1_500_000_000))
            .unwrap()
            .and_utc();
        let last_nanosecond = Utc.with_ymd_and_hms(2016, 12, 31, 23, 59, 59).unwrap()
            + TimeDelta::nanoseconds(999_999_999);

        assert_eq!(to_ntp(leap), to_ntp(last_nanosecond));
    }

    #[test]
    fn a_request_is_a_version_4_client_packet_with_its_transmit_time() {
        let packet = request(0x0123_4567_89AB_CDEF);
        assert_eq!(packet[0], 0x23);
        assert_eq!(&packet[40..48], &0x0123_4567_89AB_CDEF_u64.to_be_bytes());
        assert!(packet[1..40].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn replies_are_checked() {
        let sent = 42;
        let reply = |first: u8, stratum: u8, originate: u64, transmit: u64| {
            let mut packet = [0u8; PACKET_LEN];
            packet[0] = first;
            packet[1] = stratum;
            packet[24..32].copy_from_slice(&originate.to_be_bytes());
            packet[32..40].copy_from_slice(&5_u64.to_be_bytes());
            packet[40..48].copy_from_slice(&transmit.to_be_bytes());
            packet
        };
        let mut no_receive = reply(0x24, 1, sent, 7);
        no_receive[32..40].fill(0);
        let good = reply(0x24, 1, sent, 7);

        assert_eq!(parse_reply(&good, sent).map(|r| r.stratum), Ok(1));
        assert_eq!(parse_reply(&good[..47], sent), Err("shorter than 48 bytes"));
        assert_eq!(
            parse_reply(&reply(0x23, 1, sent, 7), sent),
            Err("not a server reply (mode 4)")
        );
        assert_eq!(
            parse_reply(&reply(0xE4, 1, sent, 7), sent),
            Err("the server is not synchronized (leap indicator 3)")
        );
        assert_eq!(
            parse_reply(&reply(0x24, 0, sent, 7), sent),
            Err("a kiss-o'-death reply (stratum 0)")
        );
        assert_eq!(
            parse_reply(&reply(0x24, 16, sent, 7), sent),
            Err("the server is not synchronized (stratum above 15)")
        );
        assert_eq!(
            parse_reply(&reply(0x24, 1, 43, 7), sent),
            Err("its originate time is not this request's")
        );
        assert_eq!(
            parse_reply(&no_receive, sent),
            Err("it has no receive time")
        );
        assert_eq!(
            parse_reply(&reply(0x24, 1, sent, 0), sent),
            Err("it has no transmit time")
        );
    }

    #[test]
    fn offset_and_round_trip_follow_rfc_4330() {
        // The server is 5 s ahead; 40 ms each way, 20 ms inside the server.
        let (offset, round_trip) = offset_and_round_trip(at(0), at(5040), at(5060), at(100));
        assert_eq!(offset, TimeDelta::seconds(5));
        assert_eq!(round_trip, TimeDelta::milliseconds(80));
    }

    #[test]
    fn server_names_get_the_ntp_port() {
        assert_eq!(with_port("time.example"), "time.example:123");
        assert_eq!(with_port("time.example:1123"), "time.example:1123");
        assert_eq!(with_port("192.0.2.1"), "192.0.2.1:123");
        assert_eq!(with_port("192.0.2.1:9"), "192.0.2.1:9");
        assert_eq!(with_port("2001:db8::1"), "[2001:db8::1]:123");
        assert_eq!(with_port("[2001:db8::1]:9"), "[2001:db8::1]:9");
    }

    #[tokio::test]
    async fn a_query_measures_the_offset_against_a_server() {
        let server = fake_server(Some((at(5040), at(5060))), 1).await;
        let clock = StepClock::new([at(0), at(100)]);

        let reading = query(&server.to_string(), &clock, Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(
            reading,
            Reading {
                stratum: 1,
                offset: TimeDelta::seconds(5),
                round_trip: TimeDelta::milliseconds(80),
            }
        );
    }

    #[tokio::test]
    async fn a_failed_address_falls_through_to_the_next() {
        // A port nobody listens on: the kernel answers the request with ICMP
        // port unreachable, which the connected socket reports as an error.
        let closed = UdpSocket::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap();
        let server = fake_server(Some((at(5040), at(5060))), 1).await;
        let clock = StepClock::new([at(0), at(0), at(100)]);

        let reading = ask_each("time.example", [closed, server], &clock)
            .await
            .unwrap();

        assert_eq!(reading.offset, TimeDelta::seconds(5));
    }

    #[tokio::test]
    async fn every_address_failing_gives_the_last_error() {
        let refusing = fake_server(Some((at(5040), at(5060))), 16).await;
        let clock = StepClock::new([at(0), at(100)]);

        let none = ask_each("time.example", [], &clock).await.unwrap_err();
        let error = ask_each("time.example", [refusing], &clock)
            .await
            .unwrap_err();

        assert!(matches!(none, NtpError::NoAddress { .. }), "{none:?}");
        assert!(matches!(error, NtpError::BadReply { .. }), "{error:?}");
    }

    #[tokio::test]
    async fn a_silent_server_times_out() {
        // Bound but never read: the request is dropped unanswered.
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = silent.local_addr().unwrap().to_string();
        let clock = StepClock::new([at(0), at(1)]);

        let error = query(&address, &clock, Duration::from_millis(200))
            .await
            .unwrap_err();

        assert!(matches!(error, NtpError::Timeout { .. }), "{error:?}");
        assert!(error.to_string().contains("did not answer"));
    }

    #[tokio::test]
    async fn an_unsynchronized_server_is_refused() {
        let server = fake_server(Some((at(5040), at(5060))), 16).await;
        let clock = StepClock::new([at(0), at(100)]);

        let error = query(&server.to_string(), &clock, Duration::from_secs(5))
            .await
            .unwrap_err();

        assert!(matches!(error, NtpError::BadReply { .. }), "{error:?}");
    }
}
