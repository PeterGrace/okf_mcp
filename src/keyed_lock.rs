//! An async mutex per key (a changeset id), created on first use and
//! dropped when the last holder or waiter lets go, so the map holds only
//! keys in use.
//!
//! [`crate::write::apply`] queues each changeset's writers here before they
//! take a pooled connection, and [`crate::overlay::Overlays`] builds each
//! changeset's overlay under one, so concurrent first reads build it once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// One async mutex per `i64` key.
#[derive(Debug, Default)]
pub struct KeyedLocks {
    // A `std` mutex: held only to look up, insert or remove an entry, never
    // across `.await`.
    locks: Mutex<HashMap<i64, Entry>>,
}

/// One key's mutex and how many callers hold or wait for it.
#[derive(Debug, Default)]
struct Entry {
    mutex: Arc<AsyncMutex<()>>,
    // An explicit count rather than `Arc::strong_count`: the clone a waiter
    // hands to `lock_owned` lives inside that pending future, whose drop
    // order on cancellation the cleanup must not depend on.
    users: usize,
}

/// Holds one key's lock; dropping it releases the lock and, if nobody else
/// holds or waits for it, removes the key's entry.
#[derive(Debug)]
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct KeyedGuard<'a> {
    // Fields drop in declaration order: the lock is released before the
    // registration tidies the map.
    _held: OwnedMutexGuard<()>,
    _registration: Registration<'a>,
}

/// One caller's count on a key's entry, from before it waits until its
/// guard is dropped. Created before the `.await`, so a waiter cancelled
/// mid-wait (its future dropped, as on a timeout or a client disconnect)
/// still gives its count back.
#[derive(Debug)]
struct Registration<'a> {
    owner: &'a KeyedLocks,
    key: i64,
}

impl KeyedLocks {
    /// An empty set of locks.
    ///
    /// # Returns
    /// The locks.
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits for `key`'s lock, in arrival order (tokio's mutex is fair).
    /// Cancel-safe: dropping the returned future before it completes leaves
    /// no entry behind.
    ///
    /// # Arguments
    /// * `key` - what to lock (a changeset id).
    ///
    /// # Returns
    /// The guard; the lock is held until it is dropped.
    pub async fn lock(&self, key: i64) -> KeyedGuard<'_> {
        let (registration, mutex) = self.register(key);
        let held = mutex.lock_owned().await;
        KeyedGuard {
            _held: held,
            _registration: registration,
        }
    }

    /// Counts one more user of `key`'s entry (creating it) and clones its
    /// mutex, both under the map lock.
    fn register(&self, key: i64) -> (Registration<'_>, Arc<AsyncMutex<()>>) {
        let mut map = self.map();
        let entry = map.entry(key).or_default();
        entry.users += 1;
        let mutex = Arc::clone(&entry.mutex);
        (Registration { owner: self, key }, mutex)
    }

    /// The number of keys with an entry (held or waited for).
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map().len()
    }

    fn map(&self) -> MutexGuard<'_, HashMap<i64, Entry>> {
        self.locks.lock().expect("keyed lock map poisoned")
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        let mut map = self.owner.map();
        // Give this caller's count back; remove the entry once nobody uses it.
        let unused = map.get_mut(&self.key).is_some_and(|entry| {
            entry.users -= 1;
            entry.users == 0
        });
        if unused {
            map.remove(&self.key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn a_key_is_held_by_one_caller_at_a_time() {
        // Arrange
        let locks = Arc::new(KeyedLocks::new());
        let first = locks.lock(1).await;

        // Act: a second caller on the same key waits; another key does not.
        let other = tokio::time::timeout(Duration::from_millis(50), locks.lock(2)).await;
        let waiting = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let _second = locks.lock(1).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        let finished_while_held = waiting.is_finished();
        let other_acquired = other.is_ok();
        drop(other);
        drop(first);
        waiting.await.unwrap();

        // Assert
        assert!(other_acquired, "another key is not held up");
        assert!(!finished_while_held, "the second caller waited");
        assert_eq!(locks.len(), 0, "every entry is removed once unused");
    }

    #[tokio::test]
    async fn an_entry_stays_while_a_waiter_needs_it() {
        // Arrange
        let locks = Arc::new(KeyedLocks::new());
        let first = locks.lock(7).await;
        let waiting = {
            let locks = Arc::clone(&locks);
            tokio::spawn(async move {
                let _second = locks.lock(7).await;
                tokio::time::sleep(Duration::from_millis(20)).await;
            })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;

        // Act
        drop(first);
        let len_with_waiter = locks.len();
        waiting.await.unwrap();

        // Assert
        assert_eq!(len_with_waiter, 1, "the waiter's entry is kept");
        assert_eq!(locks.len(), 0);
    }

    #[tokio::test]
    async fn a_waiter_cancelled_after_the_holder_leaves_removes_the_entry() {
        // Arrange: A holds the key; B is polled once, into the wait queue.
        let locks = KeyedLocks::new();
        let first = locks.lock(3).await;
        let mut waiting = Box::pin(locks.lock(3));
        // `biased` polls the arms in order: `waiting` once, then the ready
        // arm wins, leaving `waiting` queued but not dropped.
        tokio::select! {
            biased;
            _ = &mut waiting => panic!("the key is held"),
            () = std::future::ready(()) => {}
        }

        // Act: A leaves (B is still a user), then B is cancelled before it
        // is polled again.
        drop(first);
        let len_with_waiter = locks.len();
        drop(waiting);

        // Assert
        assert_eq!(len_with_waiter, 1, "the waiter's entry is kept");
        assert_eq!(locks.len(), 0, "the cancelled waiter's entry is removed");
    }
}
