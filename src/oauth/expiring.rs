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

/// A map from string keys to values that expire `ttl` after insertion,
/// holding at most `capacity` entries. Expired entries are dropped when
/// touched, and all of them at once when the map is full, so a full prune
/// runs only when needed rather than on every insert.
// `<V>`: generic over the value type, like `dict[str, V]` with a type variable.
pub struct ExpiringMap<V> {
    ttl: Duration,
    capacity: usize,
    // A `std` mutex: held only for a lookup or an insert, never across an
    // `.await` (the `SessionKeys` pattern).
    entries: Mutex<HashMap<String, Entry<V>>>,
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
            entries: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry<V>>> {
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
        if entries.len() >= self.capacity && !entries.contains_key(&key) {
            // `retain` keeps the entries the closure accepts, in place.
            entries.retain(|_, entry| entry.expires > now);
            if entries.len() >= self.capacity {
                return false;
            }
        }
        entries.insert(
            key,
            Entry {
                expires: now + self.ttl,
                value,
            },
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
        let entry = self.lock().remove(key)?;
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
        let live = entries.get(key).map(|entry| entry.expires > now)?;
        if !live {
            entries.remove(key);
            return None;
        }
        entries.get_mut(key).map(|entry| change(&mut entry.value))
    }

    /// Drops an entry, live or not.
    ///
    /// # Arguments
    /// * `key` - its key.
    pub fn remove(&self, key: &str) {
        self.lock().remove(key);
    }

    /// How many entries it holds, expired ones included.
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.lock().len()
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
    fn remove_drops_an_entry() {
        let map = ExpiringMap::new(TTL, 2);
        let start = Instant::now();
        map.insert("a".into(), 1, start);

        map.remove("a");

        assert_eq!(map.take("a", start), None);
    }
}
