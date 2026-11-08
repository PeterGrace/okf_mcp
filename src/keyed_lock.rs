//! An async mutex per key (a changeset id), created on first use and
//! dropped when the last holder or waiter lets go, so the map holds only
//! keys in use.
//!
//! [`crate::write::apply`] queues each changeset's writers here before they
//! take a pooled connection, and [`crate::overlay::Overlays`] builds each
//! changeset's overlay under one, so concurrent first reads build it once.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

/// One async mutex per `i64` key.
#[derive(Debug, Default)]
pub struct KeyedLocks {
    // A `std` mutex: held only to look up, insert or remove an entry, never
    // across `.await`. Each entry is shared (`Arc`) by its holder and every
    // waiter, so its strong count says whether anyone still needs it.
    locks: Mutex<HashMap<i64, Arc<AsyncMutex<()>>>>,
}

/// Holds one key's lock; dropping it releases the lock and, if nobody else
/// holds or waits for it, removes the key's entry.
#[derive(Debug)]
#[must_use = "the lock is released as soon as the guard is dropped"]
pub struct KeyedGuard<'a> {
    owner: &'a KeyedLocks,
    key: i64,
    // `Option` so `drop` can release the lock before tidying the map.
    guard: Option<OwnedMutexGuard<()>>,
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
    ///
    /// # Arguments
    /// * `key` - what to lock (a changeset id).
    ///
    /// # Returns
    /// The guard; the lock is held until it is dropped.
    pub async fn lock(&self, key: i64) -> KeyedGuard<'_> {
        // The clone is taken under the map lock, so `KeyedGuard::drop` (which
        // checks the count under the same lock) never removes an entry that
        // someone is about to wait on.
        let mutex = Arc::clone(self.map().entry(key).or_default());
        let guard = mutex.lock_owned().await;
        KeyedGuard {
            owner: self,
            key,
            guard: Some(guard),
        }
    }

    /// The number of keys with an entry (held or waited for).
    #[cfg(test)]
    fn len(&self) -> usize {
        self.map().len()
    }

    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<i64, Arc<AsyncMutex<()>>>> {
        self.locks.lock().expect("keyed lock map poisoned")
    }
}

impl Drop for KeyedGuard<'_> {
    fn drop(&mut self) {
        // Release first: the guard owns one count on the entry's `Arc`.
        drop(self.guard.take());
        let mut map = self.owner.map();
        // Only the map's own count left: no holder and no waiter.
        if map
            .get(&self.key)
            .is_some_and(|mutex| Arc::strong_count(mutex) == 1)
        {
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
}
