//! Global token buckets for the unauthenticated OAuth endpoints (Decision
//! 8): registration, token exchange and consent posts. Global, not per
//! address: behind an ingress the address is the proxy's.

use std::sync::Mutex;
use std::time::Instant;

/// A token bucket: `capacity` requests at once, refilled at
/// `refill_per_sec`.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    refill_per_sec: f64,
    // Tokens left and when they were counted; `None` until the first
    // request, which finds the bucket full.
    state: Mutex<Option<(f64, Instant)>>,
}

impl TokenBucket {
    /// A full bucket.
    ///
    /// # Arguments
    /// * `capacity` - the burst size.
    /// * `refill_per_sec` - the sustained rate.
    ///
    /// # Returns
    /// The bucket.
    pub fn new(capacity: u32, refill_per_sec: f64) -> Self {
        Self {
            capacity: f64::from(capacity),
            refill_per_sec,
            state: Mutex::new(None),
        }
    }

    /// Takes one token if there is one.
    ///
    /// # Arguments
    /// * `now` - the current instant (passed in, so tests control time).
    ///
    /// # Returns
    /// `true` if the request may go ahead.
    pub fn try_take(&self, now: Instant) -> bool {
        let mut state = self.state.lock().expect("token bucket lock poisoned");
        // `unwrap_or` on an `Option` of a `Copy` tuple: a full bucket the
        // first time.
        let (tokens, counted) = state.unwrap_or((self.capacity, now));
        // `saturating_duration_since` is zero, not a panic, when another
        // thread counted with a later instant than ours.
        let elapsed = now.saturating_duration_since(counted).as_secs_f64();
        let available = (tokens + elapsed * self.refill_per_sec).min(self.capacity);
        let taken = available >= 1.0;
        let left = if taken { available - 1.0 } else { available };
        *state = Some((left, now.max(counted)));
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_burst_is_allowed_then_the_rate_applies() {
        let bucket = TokenBucket::new(3, 1.0);
        let start = Instant::now();

        let burst: Vec<bool> = (0..4).map(|_| bucket.try_take(start)).collect();
        let too_soon = bucket.try_take(start + Duration::from_millis(500));
        let refilled = bucket.try_take(start + Duration::from_millis(1500));

        assert_eq!(burst, [true, true, true, false]);
        assert!(!too_soon);
        assert!(refilled);
    }

    #[test]
    fn refill_stops_at_capacity_and_tolerates_earlier_instants() {
        let bucket = TokenBucket::new(2, 10.0);
        let start = Instant::now();
        bucket.try_take(start);

        let later = start + Duration::from_secs(60);
        let taken: Vec<bool> = (0..3).map(|_| bucket.try_take(later)).collect();
        // An instant before the last count: no refill, no panic.
        let earlier = bucket.try_take(start);

        assert_eq!(taken, [true, true, false]);
        assert!(!earlier);
    }
}
