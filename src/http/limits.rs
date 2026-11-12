//! Per-key rate limits for the sign-in routes (plan Decision 8): a token
//! bucket per client address or per email, held in an [`ExpiringMap`] so
//! idle keys are dropped and memory stays bounded. In memory and per
//! process, like M10's global buckets (multi-replica limits are a
//! follow-up).

use std::net::IpAddr;
use std::time::{Duration, Instant};

use crate::oauth::expiring::ExpiringMap;
use crate::oauth::ratelimit::TokenBucket;

/// Password sign-in attempts per address per [`SIGN_IN_WINDOW`] (spec).
pub const SIGN_IN_PER_IP: u32 = 30;
/// The window of [`SIGN_IN_PER_IP`].
pub const SIGN_IN_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Reset requests per email per [`FORGOT_WINDOW`] (spec).
pub const FORGOT_PER_EMAIL: u32 = 3;
/// Reset requests per address per [`FORGOT_WINDOW`] (spec).
pub const FORGOT_PER_IP: u32 = 20;
/// The window of the reset limits.
pub const FORGOT_WINDOW: Duration = Duration::from_secs(60 * 60);
/// The most keys one limiter tracks at once: a bound on its memory (a
/// bucket and its key are well under a kilobyte, so a few tens of MiB for
/// all three limiters at worst).
pub const MAX_LIMITED_KEYS: usize = 100_000;

/// The IPv6 prefix length one limiter key covers. One host is usually
/// handed a whole /64, so per-address keys would let it spray 2^64 of them.
const IPV6_KEY_PREFIX_BITS: u32 = 64;

/// A token bucket per key: `capacity` requests at once, refilled evenly
/// over `per`.
///
/// A key's bucket is dropped `per` after it was created and starts full
/// again, so one key can get at most twice `capacity` through in any
/// window of `per`. That bound is what a fixed window gives too, and it
/// keeps the map from growing with keys that are no longer used.
pub struct KeyedLimiter {
    capacity: u32,
    refill_per_sec: f64,
    buckets: ExpiringMap<TokenBucket>,
}

impl KeyedLimiter {
    /// An empty limiter.
    ///
    /// # Arguments
    /// * `capacity` - requests per key per `per`.
    /// * `per` - the window; more than zero.
    /// * `max_keys` - the most keys tracked at once.
    ///
    /// # Returns
    /// The limiter.
    pub fn new(capacity: u32, per: Duration, max_keys: usize) -> Self {
        Self {
            capacity,
            refill_per_sec: f64::from(capacity) / per.as_secs_f64(),
            buckets: ExpiringMap::new(per, max_keys),
        }
    }

    /// Takes one request for `key` if its bucket has one.
    ///
    /// # Arguments
    /// * `key` - an address ([`ip_key`]) or a normalized email.
    /// * `now` - the current instant (passed in, so tests control time).
    ///
    /// # Returns
    /// `true` if the request may go ahead. A full map refuses a new key:
    /// spraying keys must not evict the buckets that are holding someone
    /// back.
    pub fn try_take(&self, key: &str, now: Instant) -> bool {
        // One lock covers finding or creating the key's bucket and taking
        // from it, so concurrent first requests share one bucket and no
        // late creator replaces a bucket others have drawn down. `None`
        // (a full map) refuses.
        self.buckets
            .update_or_insert_with(
                key,
                now,
                || TokenBucket::new(self.capacity, self.refill_per_sec),
                |bucket| bucket.try_take(now),
            )
            .unwrap_or(false)
    }
}

/// The limits the sign-in routes share, held by `App`.
pub struct SignInLimits {
    /// Password and passkey sign-in attempts, by address.
    pub sign_in_per_ip: KeyedLimiter,
    /// `POST /signin/forgot`, by normalized email.
    pub forgot_per_email: KeyedLimiter,
    /// `POST /signin/forgot`, by address.
    pub forgot_per_ip: KeyedLimiter,
}

impl SignInLimits {
    /// Empty limiters at the spec's rates.
    ///
    /// # Returns
    /// The limits.
    pub fn new() -> Self {
        Self {
            sign_in_per_ip: KeyedLimiter::new(SIGN_IN_PER_IP, SIGN_IN_WINDOW, MAX_LIMITED_KEYS),
            forgot_per_email: KeyedLimiter::new(FORGOT_PER_EMAIL, FORGOT_WINDOW, MAX_LIMITED_KEYS),
            forgot_per_ip: KeyedLimiter::new(FORGOT_PER_IP, FORGOT_WINDOW, MAX_LIMITED_KEYS),
        }
    }
}

impl Default for SignInLimits {
    fn default() -> Self {
        Self::new()
    }
}

/// The limiter key of a client address.
///
/// # Arguments
/// * `ip` - from `console_session::client_ip`.
///
/// # Returns
/// An IPv4 address as text; an IPv6 address as its /64 (`2001:db8::/64`),
/// so one host cannot get a bucket per address; or `"unknown"` (requests
/// with no address, such as in-process tests, share one bucket).
pub fn ip_key(ip: Option<IpAddr>) -> String {
    // `to_canonical` turns an IPv4-mapped IPv6 address (`::ffff:192.0.2.7`)
    // back into the IPv4 address it carries.
    match ip.map(|ip| ip.to_canonical()) {
        None => "unknown".to_string(),
        Some(IpAddr::V4(v4)) => v4.to_string(),
        Some(IpAddr::V6(v6)) => {
            // `u128::MAX << 64` keeps the top 64 bits (the network part).
            let network = u128::from(v6) & (u128::MAX << (128 - IPV6_KEY_PREFIX_BITS));
            format!(
                "{}/{IPV6_KEY_PREFIX_BITS}",
                std::net::Ipv6Addr::from(network)
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WINDOW: Duration = Duration::from_secs(900);

    #[test]
    fn each_key_gets_its_own_capacity() {
        let limiter = KeyedLimiter::new(3, WINDOW, 100);
        let now = Instant::now();
        for _ in 0..3 {
            assert!(limiter.try_take("a", now));
        }
        assert!(!limiter.try_take("a", now));
        assert!(
            limiter.try_take("b", now),
            "another key is not limited by the first"
        );
    }

    #[test]
    fn a_key_refills_over_its_window() {
        let limiter = KeyedLimiter::new(3, WINDOW, 100);
        let now = Instant::now();
        for _ in 0..3 {
            limiter.try_take("a", now);
        }
        // One token comes back every 300 s (3 per 900 s).
        assert!(!limiter.try_take("a", now + Duration::from_secs(200)));
        assert!(limiter.try_take("a", now + Duration::from_secs(301)));
    }

    #[test]
    fn a_full_map_refuses_new_keys_rather_than_forgetting_old_ones() {
        let limiter = KeyedLimiter::new(1, WINDOW, 2);
        let now = Instant::now();
        assert!(limiter.try_take("a", now));
        assert!(limiter.try_take("b", now));
        assert!(!limiter.try_take("c", now));
        // A refused key leaves the old ones held back.
        assert!(!limiter.try_take("a", now));
        // Once the window passes, the expired keys make room.
        assert!(limiter.try_take("c", now + WINDOW + Duration::from_secs(1)));
    }

    /// Runs `try_take(key, now)` on `callers` threads released together by
    /// a barrier, and counts how many were let through.
    fn passed_together(limiter: &KeyedLimiter, key: &str, now: Instant, callers: usize) -> usize {
        let barrier = std::sync::Barrier::new(callers);
        // `thread::scope` lets the threads borrow `limiter` and `barrier`:
        // every thread is joined before the scope returns.
        std::thread::scope(|scope| {
            let handles: Vec<_> = (0..callers)
                .map(|_| {
                    scope.spawn(|| {
                        barrier.wait();
                        limiter.try_take(key, now)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("a caller thread"))
                .filter(|passed| *passed)
                .count()
        })
    }

    #[test]
    fn concurrent_first_requests_share_one_bucket() {
        let limiter = KeyedLimiter::new(1, WINDOW, 100);
        let now = Instant::now();

        let passed = passed_together(&limiter, "a", now, 16);

        assert_eq!(passed, 1, "one token, one request through");
        assert!(!limiter.try_take("a", now), "and the next is refused");
    }

    #[test]
    fn an_expired_bucket_is_replaced_exactly_once() {
        let limiter = KeyedLimiter::new(1, WINDOW, 100);
        let start = Instant::now();
        assert!(limiter.try_take("a", start));
        let later = start + WINDOW + Duration::from_secs(1);

        let passed = passed_together(&limiter, "a", later, 16);

        assert_eq!(passed, 1, "the new bucket is shared, not made per caller");
        assert!(!limiter.try_take("a", later));
    }

    #[test]
    fn an_unknown_address_shares_one_key() {
        assert_eq!(ip_key(None), "unknown");
        assert_eq!(
            ip_key(Some("192.0.2.7".parse().expect("an address"))),
            "192.0.2.7"
        );
    }

    #[test]
    fn an_ipv6_host_gets_one_key_for_its_whole_64() {
        let first = ip_key(Some("2001:db8:1:2::1".parse().expect("an address")));
        let second = ip_key(Some(
            "2001:db8:1:2:ffff:ffff:ffff:ffff"
                .parse()
                .expect("an address"),
        ));
        let elsewhere = ip_key(Some("2001:db8:1:3::1".parse().expect("an address")));

        assert_eq!(first, "2001:db8:1:2::/64");
        assert_eq!(first, second);
        assert_ne!(first, elsewhere);
        assert_eq!(
            ip_key(Some("::ffff:192.0.2.7".parse().expect("an address"))),
            "192.0.2.7",
            "an IPv4-mapped address is its IPv4 address"
        );
    }
}
