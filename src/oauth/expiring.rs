//! A bounded map whose entries expire: the in-memory pending
//! authorizations and authorization codes (spec "In memory"). The server is
//! single-replica, so a restart costs at most a consent page in progress.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// One value and when it stops being returned.
struct Entry<V> {
    expires: Instant,
    value: V,
}

/// The entries and a lower bound on their expiries.
struct Entries<V> {
    by_key: HashMap<String, Entry<V>>,
    // No entry expires before this (`None` only while empty). Removing an
    // entry leaves it a valid lower bound, so only inserts and prunes move it.
    earliest_expiry: Option<Instant>,
}

/// A map from string keys to values that expire `ttl` after insertion,
/// holding at most `capacity` entries. Expired entries are dropped when
/// touched, and all of them at once when the map is full, so a full prune
/// runs only when needed rather than on every insert. A full map whose
/// earliest entry has not expired yet refuses a new key without scanning,
/// so a flood of new keys against a full map costs one lookup each.
// `<V>`: generic over the value type, like `dict[str, V]` with a type variable.
pub struct ExpiringMap<V> {
    ttl: Duration,
    capacity: usize,
    // A `std` mutex: held only for a lookup or an insert, never across an
    // `.await` (the `SessionKeys` pattern).
    entries: Mutex<Entries<V>>,
}

impl<V> ExpiringMap<V> {
    /// An empty map.
    ///
    /// # Arguments
    /// * `ttl` - how long an entry lives.
    /// * `capacity` - the most live entries it holds.
    ///
    /// # Returns
    /// The map.
    pub fn new(ttl: Duration, capacity: usize) -> Self {
        Self {
            ttl,
            capacity,
            entries: Mutex::new(Entries {
                by_key: HashMap::new(),
                earliest_expiry: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Entries<V>> {
        self.entries.lock().expect("expiring map lock poisoned")
    }

    /// Stores `value` under `key` until `now + ttl`, replacing any entry of
    /// that key.
    ///
    /// # Arguments
    /// * `key` - its key (a random id or a hash).
    /// * `value` - the value.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// `false`, storing nothing, when the map holds `capacity` live entries.
    pub fn insert(&self, key: String, value: V, now: Instant) -> bool {
        let mut entries = self.lock();
        if entries.by_key.len() >= self.capacity && !entries.by_key.contains_key(&key) {
            // Nothing can have expired yet: refuse without a scan.
            if entries
                .earliest_expiry
                .is_some_and(|earliest| earliest > now)
            {
                return false;
            }
            // `retain` keeps the entries the closure accepts, in place; the
            // closure also finds the earliest expiry left.
            let mut earliest: Option<Instant> = None;
            entries.by_key.retain(|_, entry| {
                let live = entry.expires > now;
                if live {
                    earliest = Some(earliest.map_or(entry.expires, |at| at.min(entry.expires)));
                }
                live
            });
            entries.earliest_expiry = earliest;
            if entries.by_key.len() >= self.capacity {
                return false;
            }
        }
        let expires = now + self.ttl;
        entries.by_key.insert(key, Entry { expires, value });
        entries.earliest_expiry = Some(
            entries
                .earliest_expiry
                .map_or(expires, |earliest| earliest.min(expires)),
        );
        true
    }

    /// Removes and returns a live entry: a single-use read.
    ///
    /// # Arguments
    /// * `key` - its key.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// The value, or `None` if absent or expired (an expired one is dropped).
    pub fn take(&self, key: &str, now: Instant) -> Option<V> {
        let entry = self.lock().by_key.remove(key)?;
        // `then_some` is `Some(value) if condition else None`.
        (entry.expires > now).then_some(entry.value)
    }

    /// Changes a live entry in place.
    ///
    /// # Arguments
    /// * `key` - its key.
    /// * `now` - the current instant.
    /// * `change` - what to do with it; its result is returned.
    ///
    /// # Returns
    /// `change`'s result, or `None` if the entry is absent or expired (an
    /// expired one is dropped).
    // `impl FnOnce(&mut V) -> R`: any closure called once with a mutable
    // borrow of the value, like a Python callback that mutates its argument.
    pub fn update<R>(
        &self,
        key: &str,
        now: Instant,
        change: impl FnOnce(&mut V) -> R,
    ) -> Option<R> {
        let mut entries = self.lock();
        let live = entries.by_key.get(key).map(|entry| entry.expires > now)?;
        if !live {
            entries.by_key.remove(key);
            return None;
        }
        entries
            .by_key
            .get_mut(key)
            .map(|entry| change(&mut entry.value))
    }

    /// Drops an entry, live or not.
    ///
    /// # Arguments
    /// * `key` - its key.
    pub fn remove(&self, key: &str) {
        self.lock().by_key.remove(key);
    }

    /// How many entries it holds, expired ones included.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().by_key.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(60);

    #[test]
    fn take_returns_a_live_entry_once() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        assert!(map.insert("a".into(), 1, start));

        let first = map.take("a", start + Duration::from_secs(59));
        let second = map.take("a", start + Duration::from_secs(59));

        assert_eq!((first, second), (Some(1), None));
    }

    #[test]
    fn expired_entries_are_not_returned_and_are_dropped() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        map.insert("a".into(), 1, start);
        map.insert("b".into(), 2, start);

        assert_eq!(map.take("a", start + TTL), None);
        assert_eq!(map.update("b", start + TTL, |value| *value), None);
        assert_eq!(map.len(), 0);
    }

    #[test]
    fn update_changes_a_live_entry_in_place() {
        let map = ExpiringMap::new(TTL, 4);
        let start = Instant::now();
        map.insert("a".into(), 1, start);

        let seen = map.update("a", start, |value| {
            *value += 1;
            *value
        });

        assert_eq!(seen, Some(2));
        assert_eq!(map.take("a", start), Some(2));
        assert_eq!(map.update("missing", start, |value: &mut i32| *value), None);
    }

    #[test]
    fn a_full_map_prunes_expired_entries_then_refuses() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        assert!(map.insert("a".into(), 1, start));
        assert!(map.insert("b".into(), 2, start + Duration::from_secs(30)));

        // Full of live entries: refused, and nothing is dropped.
        assert!(!map.insert("c".into(), 3, start + Duration::from_secs(31)));
        // Replacing an existing key is always allowed.
        assert!(map.insert("b".into(), 4, start + Duration::from_secs(31)));
        // "a" has expired: the prune makes room.
        assert!(map.insert("c".into(), 3, start + TTL));

        assert_eq!(map.len(), 2);
        assert_eq!(map.take("c", start + TTL), Some(3));
        assert_eq!(map.take("b", start + TTL), Some(4));
    }

    #[test]
    fn a_full_map_tracks_its_earliest_expiry_through_removals_and_prunes() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        map.insert("a".into(), 1, start);
        map.insert("b".into(), 2, start + Duration::from_secs(10));
        // Taking "a" leaves the bound at its old expiry, which is still a
        // lower bound: the next prune recomputes it.
        map.take("a", start);
        assert!(map.insert("c".into(), 3, start + Duration::from_secs(20)));
        // Full of live entries: refused, and the bound is now "b"'s expiry.
        assert!(!map.insert("d".into(), 4, start + TTL));
        assert_eq!(
            map.lock().earliest_expiry,
            Some(start + Duration::from_secs(10) + TTL)
        );
        // Before it, a new key is refused without a scan; from it, "b" goes.
        assert!(!map.insert("d".into(), 4, start + Duration::from_secs(9) + TTL));
        assert!(map.insert("d".into(), 4, start + Duration::from_secs(10) + TTL));
        assert_eq!(map.take("c", start + TTL), Some(3));
        assert_eq!(map.take("b", start + TTL), None);
    }

    #[test]
    fn remove_drops_an_entry() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        map.insert("a".into(), 1, start);

        map.remove("a");

        assert_eq!(map.take("a", start), None);
    }
}
