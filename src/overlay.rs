//! Overlays (M5 Decision 6): a connection's view of a catalog with its open
//! changeset applied, so its reads see its own pending edits.
//!
//! An overlay is the published workspace with the changeset's entries
//! applied and a full snapshot of the result (about 10 ms for the real
//! catalog), built on a blocking thread. [`Overlays`] caches one per
//! changeset, valid while the changeset's `version` and the published
//! `generation` stay the same; no incremental snapshot.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::PgExecutor;

use crate::catalog::{CatalogState, Loaded};
use crate::error::OkfError;
use crate::mcp::blocking;
use crate::okf::analysis::lint::{lint, CommitTimes, LintReport};
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::changesets::{self, ChangesetRow, EntryRow};

/// A catalog's published state with one changeset's entries applied.
/// Immutable.
#[derive(Debug)]
pub struct Overlay {
    base: Arc<Loaded>,
    changeset_id: i64,
    version: i64,
    workspace: MemWorkspace,
    snapshot: Snapshot,
    dirty: BTreeSet<String>,
    // The base's commit times with `dirty` marked uncommitted, made once.
    commit_times: CommitTimes,
}

impl Overlay {
    /// The snapshot of the overlaid files.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// The overlaid files (what a write op reads, through a copy).
    pub fn workspace(&self) -> &MemWorkspace {
        &self.workspace
    }

    /// The published state it was built on.
    pub fn base(&self) -> &Arc<Loaded> {
        &self.base
    }

    /// The changeset's paths, sorted (deleted ones included).
    pub fn dirty(&self) -> &BTreeSet<String> {
        &self.dirty
    }

    /// The changeset it shows.
    pub fn changeset_id(&self) -> i64 {
        self.changeset_id
    }

    /// The changeset `version` it was built at.
    pub fn version(&self) -> i64 {
        self.version
    }

    /// Lints the overlaid snapshot, the changeset's paths counted as
    /// uncommitted (`lint pending`).
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    ///
    /// # Returns
    /// The findings, as [`lint`] returns them.
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        lint(&self.snapshot, now, Some(&self.commit_times))
    }
}

/// Builds an overlay on a blocking thread.
///
/// # Arguments
/// * `base` - the published state.
/// * `changeset` - the changeset (its id and `version` are recorded).
/// * `entries` - its entries; an entry with `content: None` deletes its path.
///
/// # Returns
/// The overlay.
///
/// # Errors
/// [`OkfError::InvalidPath`] if the entries do not form a tree with the
/// published files, or if the snapshot cannot read a pending file (a `.md`
/// that is not UTF-8), its reason naming the changeset (every read through
/// this overlay fails until the changeset is fixed, so the error says where
/// to look); any other snapshot error, as for a published catalog.
pub async fn build_overlay(
    base: Arc<Loaded>,
    changeset: &ChangesetRow,
    entries: Vec<EntryRow>,
) -> Result<Overlay, OkfError> {
    let changeset_id = changeset.id;
    let version = changeset.version;
    // Owned, so the blocking closure can take it: errors name the handle,
    // which is what a client knows the changeset by (M8 Decision 1).
    let handle = changeset.handle.clone();
    // `move` hands `base` and `entries` to the blocking thread, which must
    // own what it reads.
    blocking(move || {
        let dirty: BTreeSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();
        let changes = entries.into_iter().map(|entry| {
            let executable = entry.executable;
            let file = entry.content.map(|content| StoredFile {
                content,
                executable,
            });
            (entry.path, file)
        });
        let workspace = base
            .workspace()
            .with_changes(changes)
            .map_err(|error| in_changeset(error, &handle))?;
        // The published files built into the base snapshot, so a failure
        // here comes from the pending entries (a `.md` whose bytes are not
        // UTF-8): named as theirs, like a clash.
        let snapshot = Snapshot::build(&workspace).map_err(|error| in_changeset(error, &handle))?;
        let commit_times = base.commit_times().with_dirty(dirty.iter().cloned());
        Ok(Overlay {
            base,
            changeset_id,
            version,
            workspace,
            snapshot,
            dirty,
            commit_times,
        })
    })
    .await
}

/// Turns an error that applying a changeset's entries caused into an
/// [`OkfError::InvalidPath`] that names the changeset and how to recover, so
/// it is not masked as internal and a read can tell it from a database
/// failure ([`crate::mcp::context::is_broken_overlay`]).
///
/// # Arguments
/// * `error` - the error from applying the entries or building the
///   snapshot of the result.
/// * `handle` - the handle of the changeset whose entries were applied.
///
/// # Returns
/// An `InvalidPath` (a clash) or an `Io` (a pending file the snapshot cannot
/// read) as an `InvalidPath` with the changeset in its reason; any other
/// error as it is.
fn in_changeset(error: OkfError, handle: &str) -> OkfError {
    // Each `format!` runs before the struct is built, because building it
    // moves `path` into the struct.
    match error {
        // The clash is reported at the file's path, which is either the
        // pending file or a published one with pending paths under it.
        OkfError::InvalidPath { path, reason } => {
            let reason = format!(
                "{reason}, in pending changeset {handle}; discard the clashing path \
                 ({path} or the paths under it) or the changeset"
            );
            OkfError::InvalidPath { path, reason }
        }
        OkfError::Io { path, source } => {
            let reason =
                format!("{source}, in pending changeset {handle}; discard {path} or the changeset");
            OkfError::InvalidPath { path, reason }
        }
        other => other,
    }
}

/// What an overlay was built at: the base `generation`, then the changeset
/// `version`. Tuples compare field by field, like Python's, so a later
/// generation always counts as newer.
fn built_at(overlay: &Overlay) -> (i64, i64) {
    (overlay.base.generation(), overlay.version)
}

/// A cached overlay and when it was last used.
#[derive(Debug)]
struct Cached {
    overlay: Arc<Overlay>,
    used: Instant,
}

/// Every connection's overlay, by changeset id.
#[derive(Debug)]
pub struct Overlays {
    idle: Duration,
    // A `std` mutex: held for a lookup or an insert, never across `.await`.
    cached: Mutex<HashMap<i64, Cached>>,
}

impl Overlays {
    /// An empty cache whose entries are dropped after `idle` without use.
    ///
    /// # Arguments
    /// * `idle` - `session_idle_timeout`.
    ///
    /// # Returns
    /// The cache.
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            cached: Mutex::new(HashMap::new()),
        }
    }

    /// The overlay of `changeset` on `state`'s current published state:
    /// the cached one if it was built at this `version` and `generation`,
    /// else a new one (its entries read now).
    ///
    /// # Arguments
    /// * `executor` - where to read the entries: the pool for a read, the
    ///   write's own transaction for a write (`&mut *tx`), so a write that
    ///   holds the changeset's row lock needs no second connection.
    /// * `state` - the changeset's catalog.
    /// * `changeset` - an open changeset, as just read.
    ///
    /// # Returns
    /// The overlay, shared.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the entries; as [`build_overlay`].
    // `impl PgExecutor<'_>` takes a `&PgPool` or a `&mut PgConnection`
    // alike (a pool, a connection or a transaction), like a duck-typed
    // "anything with execute()" argument in Python, checked at compile time.
    pub async fn get(
        &self,
        executor: impl PgExecutor<'_>,
        state: &CatalogState,
        changeset: &ChangesetRow,
    ) -> Result<Arc<Overlay>, OkfError> {
        let base = state.current();
        let now = Instant::now();
        if let Some(overlay) = self.lookup(changeset, base.generation(), now) {
            return Ok(overlay);
        }
        // Read after the caller's `find_open`, so the entries may be newer
        // than `changeset.version`; harmless: the overlay is labelled with the
        // older version, and the next read at the newer one rebuilds it.
        let entries = changesets::entries(executor, changeset.id).await?;
        let overlay = Arc::new(build_overlay(base, changeset, entries).await?);
        self.store(Arc::clone(&overlay), now);
        Ok(overlay)
    }

    /// Drops a changeset's overlay (after a write, publish or discard).
    ///
    /// # Arguments
    /// * `changeset_id` - the changeset.
    pub fn forget(&self, changeset_id: i64) {
        self.lock().remove(&changeset_id);
    }

    /// The cached overlay, if it is current; marks it used.
    ///
    /// Every read comes through here, so it is also where the cache stays
    /// bounded: it first drops every idle entry, and it drops this
    /// changeset's entry if it is older than the one asked for (each entry
    /// pins its base `Loaded`, so a stale one would keep an old generation in
    /// memory). A newer entry stays, for the callers that read the newer row.
    fn lookup(
        &self,
        changeset: &ChangesetRow,
        generation: i64,
        now: Instant,
    ) -> Option<Arc<Overlay>> {
        let mut cached = self.lock();
        self.drop_idle(&mut cached, now);
        let entry = cached.get_mut(&changeset.id)?;
        let wanted = (generation, changeset.version);
        // `cmp` on tuples compares field by field, like Python's.
        match built_at(&entry.overlay).cmp(&wanted) {
            Ordering::Equal => {
                entry.used = now;
                Some(Arc::clone(&entry.overlay))
            }
            Ordering::Less => {
                cached.remove(&changeset.id);
                None
            }
            Ordering::Greater => None,
        }
    }

    /// Caches `overlay`, unless a newer one for the same changeset is there
    /// (a slower build finishing last), first dropping every idle entry.
    fn store(&self, overlay: Arc<Overlay>, now: Instant) {
        let mut cached = self.lock();
        self.drop_idle(&mut cached, now);
        let newer_cached = cached
            .get(&overlay.changeset_id)
            .is_some_and(|entry| built_at(&entry.overlay) > built_at(&overlay));
        if !newer_cached {
            cached.insert(overlay.changeset_id, Cached { overlay, used: now });
        }
    }

    /// Drops every entry unused for longer than `idle`.
    fn drop_idle(&self, cached: &mut HashMap<i64, Cached>, now: Instant) {
        cached.retain(|_, entry| now.saturating_duration_since(entry.used) <= self.idle);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<i64, Cached>> {
        self.cached.lock().expect("overlay cache lock poisoned")
    }
}

/// What one tool call reads of one catalog.
// Both arms hold an `Arc`, so a `View` is cheap to move to a blocking thread.
#[derive(Debug, Clone)]
pub enum View {
    /// The published state: no open changeset for this key and label.
    Published(Arc<Loaded>),
    /// This connection's overlay.
    Pending(Arc<Overlay>),
}

impl View {
    /// The snapshot to read.
    pub fn snapshot(&self) -> &Snapshot {
        match self {
            View::Published(loaded) => loaded.snapshot(),
            View::Pending(overlay) => overlay.snapshot(),
        }
    }

    /// Lints what this view shows, with its commit times.
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    ///
    /// # Returns
    /// The findings.
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        match self {
            View::Published(loaded) => loaded.lint(now),
            View::Pending(overlay) => overlay.lint(now),
        }
    }

    /// Does this view show pending edits?
    pub fn pending(&self) -> bool {
        matches!(self, View::Pending(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::files;
    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, open, pending_entry, principal_as, store_key, PendingEdit,
    };
    use crate::okf::workspace::Workspace;
    use crate::store::changesets::ChangesetState;
    use crate::store::import::{import, ImportOptions};
    use crate::store::keys::Access;

    fn entry(path: &str, text: Option<&str>) -> EntryRow {
        EntryRow {
            path: path.to_string(),
            base_revision: None,
            content: text.map(|text| text.as_bytes().to_vec()),
            executable: false,
            replay: None,
        }
    }

    /// An open changeset row, never stored: for overlays built directly.
    fn changeset(id: i64, version: i64) -> ChangesetRow {
        ChangesetRow {
            id,
            handle: format!("handle-{id}"),
            catalog_id: 1,
            label: "default".to_string(),
            key_id: None,
            actor: "t".to_string(),
            state: ChangesetState::Open,
            version,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_overlay_shows_the_entries_over_the_published_files() {
        let test = app_with(
            &[("pg", &[("a.md", "# Old\n"), ("b.md", "# B\n")])],
            Config::default(),
        )
        .await;
        let base = test.app.catalog("pg").unwrap().current();
        let changeset = changeset(7, 3);
        let entries = vec![
            entry("a.md", Some("# New\n")),
            entry("b.md", None),
            entry("c.md", Some("# C\n")),
        ];

        let overlay = build_overlay(Arc::clone(&base), &changeset, entries)
            .await
            .unwrap();

        let snapshot = overlay.snapshot();
        assert_eq!(snapshot.get("a.md").unwrap().text(), "# New\n");
        assert!(snapshot.get("b.md").is_none(), "a None entry deletes");
        assert_eq!(snapshot.get("c.md").unwrap().text(), "# C\n");
        let dirty: Vec<&str> = overlay.dirty().iter().map(String::as_str).collect();
        assert_eq!(dirty, ["a.md", "b.md", "c.md"]);
        assert_eq!((overlay.changeset_id(), overlay.version()), (7, 3));
        assert!(Arc::ptr_eq(overlay.base(), &base));
        // The published state is untouched.
        assert_eq!(base.snapshot().get("a.md").unwrap().text(), "# Old\n");
        assert!(base.workspace().is_file("b.md"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cache_is_reused_until_the_version_or_the_generation_moves() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        store_key(&test, "writer").await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        let state = test.app.catalog("pg").unwrap();
        let pool = test.app.pool();
        let overlays = test.app.overlays();
        let edit = |text: &'static str| PendingEdit {
            path: "a.md",
            text: Some(text),
            at: Utc::now(),
        };
        let handle = open(&ctx).await;
        let first_row = pending_entry(&test, &handle, &edit("# One\n")).await;

        let first = overlays.get(pool, &state, &first_row).await.unwrap();
        let again = overlays.get(pool, &state, &first_row).await.unwrap();
        let second_row = pending_entry(&test, &handle, &edit("# Two\n")).await;
        let after_edit = overlays.get(pool, &state, &second_row).await.unwrap();
        let options = ImportOptions {
            replace: true,
            actor: "t",
            now: Utc::now(),
        };
        let republished = files(&[("a.md", "# A\n"), ("b.md", "# B\n")]);
        import(pool, "pg", republished, &options).await.unwrap();
        state.refresh(pool).await.unwrap();
        let after_import = overlays.get(pool, &state, &second_row).await.unwrap();
        overlays.forget(second_row.id);
        let after_forget = overlays.get(pool, &state, &second_row).await.unwrap();

        assert!(
            Arc::ptr_eq(&first, &again),
            "same version: the cached overlay"
        );
        assert!(!Arc::ptr_eq(&again, &after_edit));
        assert_eq!(after_edit.snapshot().get("a.md").unwrap().text(), "# Two\n");
        assert!(!Arc::ptr_eq(&after_edit, &after_import));
        assert!(
            after_import.snapshot().get("b.md").is_some(),
            "rebuilt on the new base"
        );
        assert_eq!(
            after_import.snapshot().get("a.md").unwrap().text(),
            "# Two\n"
        );
        assert!(!Arc::ptr_eq(&after_import, &after_forget));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_clashing_entry_names_the_pending_changeset() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        let base = test.app.catalog("pg").unwrap().current();

        let result =
            build_overlay(base, &changeset(9, 1), vec![entry("a.md/b.md", Some("x"))]).await;

        let Err(OkfError::InvalidPath { reason, .. }) = result else {
            panic!("expected InvalidPath, got {result:?}");
        };
        assert!(
            reason.ends_with(
                "in pending changeset handle-9; discard the clashing path (a.md or the paths under \
                 it) or the changeset"
            ),
            "{reason}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_cache_drops_idle_and_stale_entries_and_keeps_the_newer_build() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        let base = test.app.catalog("pg").unwrap().current();
        let generation = base.generation();
        let build = |id, version| {
            let base = Arc::clone(&base);
            async move {
                let built = build_overlay(base, &changeset(id, version), Vec::new()).await;
                Arc::new(built.unwrap())
            }
        };
        let (newer, older, other) = (build(1, 2).await, build(1, 1).await, build(2, 1).await);
        let overlays = Overlays::new(Duration::from_secs(60));
        let start = Instant::now();
        let later = start + Duration::from_secs(61);

        overlays.store(Arc::clone(&newer), start);
        overlays.store(Arc::clone(&older), start);
        let behind = overlays.lookup(&changeset(1, 1), generation, start);
        let kept = overlays.lookup(&changeset(1, 2), generation, start);
        overlays.store(Arc::clone(&other), start);
        let stale = overlays.lookup(&changeset(2, 5), generation, start);
        let stale_dropped = !overlays.lock().contains_key(&2);
        overlays.store(Arc::clone(&other), start);
        let after_idle = overlays.lookup(&changeset(3, 1), generation, later);

        assert!(
            kept.is_some_and(|kept| Arc::ptr_eq(&kept, &newer)),
            "an older build never replaces a newer one"
        );
        assert!(
            behind.is_none(),
            "an older row does not get the newer build"
        );
        assert!(stale.is_none());
        assert!(stale_dropped, "an out-of-date entry is dropped on lookup");
        assert!(after_idle.is_none());
        assert!(overlays.lock().is_empty(), "a lookup drops idle entries");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_pending_md_that_is_not_utf8_names_the_changeset() {
        let test = app_with(&[("pg", &[("a.md", "# A\n")])], Config::default()).await;
        let base = test.app.catalog("pg").unwrap().current();
        let garbled = EntryRow {
            path: "bad.md".to_string(),
            base_revision: None,
            content: Some(b"# \xff\xfe\n".to_vec()),
            executable: false,
            replay: None,
        };

        let result = build_overlay(base, &changeset(4, 1), vec![garbled]).await;

        let Err(error) = result else {
            panic!("expected an error, got {result:?}");
        };
        // Not masked as internal, so `search` can skip it.
        assert!(crate::mcp::context::is_broken_overlay(&error));
        let message = error.to_string();
        assert!(message.starts_with("bad.md: "), "{message}");
        assert!(
            message.ends_with("in pending changeset handle-4; discard bad.md or the changeset"),
            "{message}"
        );
    }
}
