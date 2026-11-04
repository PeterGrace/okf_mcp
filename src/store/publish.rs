//! Publish (design "Data flow", Publish): a changeset's entries become the
//! catalog's published documents, under the catalog's publish lock.
//!
//! The lock is two locks, taken in this order. First the in-process mutex
//! ([`CatalogState::publish_lock`]), held until the snapshot is swapped.
//! Then the advisory lock in the transaction, which a CLI import takes too.
//! The transaction then takes the catalog row (`lock_catalog`) and the
//! changeset row (`changesets::lock`), in the order every changeset
//! transaction uses. While the locks are held, nothing else can change the
//! catalog's documents or this changeset's entries, so the checks see
//! exactly what is written.

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use crate::app::App;
use crate::catalog::{CatalogState, Loaded};
use crate::error::{ConflictFile, OkfError};
use crate::mcp::blocking;
use crate::okf::analysis::lint::LintReport;
use crate::okf::analysis::Finding;
use crate::okf::ops::replay::{replay, ReplayOp};
use crate::okf::rules;
use crate::okf::snapshot::Snapshot;
use crate::overlay::{build_overlay, Overlay};
use crate::store::catalogs::{self, is_unique_violation};
use crate::store::changesets::{self, ChangesetRow, ChangesetState, EntryRow};
use crate::store::documents::{self, Derived, DocumentWrite, RevisionOp, RevisionRecord};

/// What a publish did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PublishReport {
    /// `false` when the changeset held nothing to publish.
    pub published: bool,
    /// The revisions written, in path order. A file whose content ended up as published gets none.
    pub revisions: Vec<PublishedRevision>,
    /// Lint advice: errors in files this publish does not change, and warnings in those it does.
    pub warnings: Vec<Finding>,
    /// `/`-prefixed paths whose bookkeeping (log, index listings) was replayed onto newer text.
    pub replayed: Vec<String>,
}

/// One revision a publish wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PublishedRevision {
    /// `/`-prefixed path.
    pub path: String,
    /// Its new revision.
    pub revision: i32,
    /// `create`, `update` or `delete`.
    pub op: &'static str,
}

/// A changed path's row in `documents`, tombstone or live.
#[derive(Debug)]
struct CurrentRow {
    content: Vec<u8>,
    executable: bool,
    revision: i32,
    live: bool,
}

/// The entries once replayed onto current text.
#[derive(Debug)]
struct Merged {
    entries: Vec<EntryRow>,
    replayed: Vec<String>,
}

/// The publish's identity: what every row it writes records.
#[derive(Debug)]
struct Publication<'a> {
    changeset: &'a ChangesetRow,
    actor: &'a str,
    message: &'a str,
    now: DateTime<Utc>,
}

/// Publishes a changeset (design "Publish", steps 1 to 6).
///
/// # Arguments
/// * `app` - the shared state (pool, clock, overlays).
/// * `state` - the changeset's catalog.
/// * `changeset` - the open changeset (read without its lock; it is locked
///   and read again here).
/// * `actor` - the publishing key's actor, recorded on every document and
///   revision (an admin publishing another key's changeset records its own
///   `actor`, not the opener's).
/// * `message` - recorded on every revision.
///
/// # Returns
/// The revisions written, lint advice and the replayed paths;
/// `published: false` if the changeset held no entries.
///
/// # Errors
/// - [`OkfError::Conflict`] for entries whose base is stale and that cannot
///   be replayed (a deletion entry with no base names a path published
///   after the changeset's first edit, so it is one too);
/// - [`OkfError::SecretDetected`] or [`OkfError::FrontmatterParse`] from the
///   content rules;
/// - [`OkfError::LintFailed`] for lint errors in changed files or links
///   into deleted paths;
/// - [`OkfError::Refused`] if the changeset is no longer open, or the
///   catalog was deleted while this waited;
/// - [`OkfError::CatalogNotSelected`] if the catalog was deleted;
/// - [`OkfError::Busy`] if the snapshot could not be brought up to date;
/// - an overlay that no longer applies ([`OkfError::InvalidPath`]);
/// - [`OkfError::Database`].
///
/// On every error the transaction rolls back and the changeset stays as it was.
pub async fn publish(
    app: &App,
    state: &CatalogState,
    changeset: &ChangesetRow,
    actor: &str,
    message: &str,
) -> Result<PublishReport, OkfError> {
    // Step 1, before any lock: an empty changeset never waits for one.
    if changesets::entries(app.pool(), changeset.id)
        .await?
        .is_empty()
    {
        return Ok(PublishReport::default());
    }
    // Step 2. The guard (`_publishing`) releases the mutex when it goes out
    // of scope at the end of the function, after the refresh.
    let _publishing = state.publish_lock().lock().await;
    let now = app.clock().now();
    // Dropping `tx` without `commit()` (any `?` or early return) rolls back.
    let mut tx = app.pool().begin().await?;
    catalogs::lock(&mut tx, state.id()).await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let locked = changesets::lock(&mut tx, changeset.id)
        .await?
        .filter(|row| row.state == ChangesetState::Open)
        .ok_or_else(|| OkfError::Refused {
            reason: format!("changeset {} is no longer open", changeset.id),
        })?;
    // Again under the lock: a discard may have emptied it meanwhile.
    let entries = changesets::entries(&mut *tx, locked.id).await?;
    if entries.is_empty() {
        return Ok(PublishReport::default());
    }
    let base = current_base(app, state, &mut tx).await?;

    // Step 3: base revisions against the documents as stored now.
    let paths: Vec<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let current = current_rows(&mut tx, state.id(), &paths).await?;
    let merged = merge(entries, &current)?;

    // Step 4: the rules, lint and links on the result.
    let (overlay, warnings) = check(base, &locked, merged.entries, now).await?;

    // Step 5: documents, revisions, generation, state; one commit.
    let publication = Publication {
        changeset: &locked,
        actor,
        message,
        now,
    };
    let revisions = match write_all(&mut tx, &publication, &overlay, &current, &paths).await {
        Ok(revisions) => revisions,
        // The backstop's conflict: roll back first, so the re-read below
        // sees only what is committed, and report the revision there now.
        Err(OkfError::Conflict { files }) => {
            tx.rollback().await?;
            let files = with_current_revisions(app.pool(), state.id(), files).await?;
            return Err(OkfError::Conflict { files });
        }
        Err(error) => return Err(error),
    };
    if !revisions.is_empty() {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(state.id())
            .execute(&mut *tx)
            .await?;
    }
    changesets::set_state(&mut tx, locked.id, ChangesetState::Published, now).await?;
    tx.commit().await?;

    // Step 6, still under the in-process lock (the advisory lock ended with
    // the transaction): the next publish of this catalog checks against
    // this one's result. A failed refresh does not undo a committed publish;
    // the poll loop retries it, and the next publish refreshes first.
    if let Err(error) = state.refresh(app.pool()).await {
        tracing::error!(
            catalog = %state.name(),
            %error,
            "refresh after publish failed; the poll loop retries"
        );
    }
    app.overlays().forget(locked.id);
    Ok(PublishReport {
        published: true,
        revisions,
        warnings,
        replayed: merged.replayed,
    })
}

/// The snapshot at the catalog's current generation (design "Publish",
/// step 2): rebuilt first if the database moved past it.
///
/// # Errors
/// As [`CatalogState::refresh`]; [`OkfError::Busy`] if the snapshot still
/// does not match (only a refresh failing silently could cause it).
async fn current_base(
    app: &App,
    state: &CatalogState,
    conn: &mut PgConnection,
) -> Result<Arc<Loaded>, OkfError> {
    let generation: i64 = sqlx::query_scalar("SELECT generation FROM catalogs WHERE id = $1")
        .bind(state.id())
        .fetch_one(&mut *conn)
        .await?;
    // The advisory lock keeps every other import and publish out, so the
    // refresh (on its own connection) builds exactly this generation.
    // That is a second pooled connection while this transaction holds the
    // first. Publishes of one catalog are serialized by the in-process
    // mutex, so only many catalogs publishing at once on a small pool (8
    // by default) could exhaust it, until sqlx's acquire timeout fails a
    // publish (design "Publish").
    if state.current().generation() < generation {
        state.refresh(app.pool()).await?;
    }
    let base = state.current();
    if base.generation() == generation {
        Ok(base)
    } else {
        Err(OkfError::Busy {
            reason: format!(
                "catalog {:?}'s snapshot is behind the database",
                state.name()
            ),
        })
    }
}

/// The changed paths' rows in `documents` (tombstones included), by path.
///
/// # Errors
/// [`OkfError::Database`].
async fn current_rows(
    conn: &mut PgConnection,
    catalog_id: i64,
    paths: &[String],
) -> Result<HashMap<String, CurrentRow>, OkfError> {
    // `= ANY($2)` matches any element of the bound array: one query for all
    // changed paths, served by the primary key.
    let rows: Vec<(String, Vec<u8>, bool, i32, bool)> = sqlx::query_as(
        "SELECT path, content, executable, revision, deleted_at IS NULL FROM documents \
         WHERE catalog_id = $1 AND path = ANY($2)",
    )
    .bind(catalog_id)
    .bind(paths)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(path, content, executable, revision, live)| {
            let row = CurrentRow {
                content,
                executable,
                revision,
                live,
            };
            (path, row)
        })
        .collect())
}

/// Checks each entry's base against the current revision (design "Publish",
/// step 3). A fresh entry is kept. A stale one with a replay has its
/// operations redone on the current text. Any other stale one is a conflict.
///
/// # Errors
/// [`OkfError::Conflict`] listing every conflicting file; a replay's error.
fn merge(
    entries: Vec<EntryRow>,
    current: &HashMap<String, CurrentRow>,
) -> Result<Merged, OkfError> {
    let mut merged = Vec::with_capacity(entries.len());
    let mut replayed = Vec::new();
    let mut conflicts = Vec::new();
    // `mut entry`: the loop owns each entry and may rewrite its content.
    for mut entry in entries {
        let live = current.get(&entry.path).filter(|row| row.live);
        let current_revision = live.map(|row| row.revision);
        if entry.base_revision == current_revision {
            merged.push(entry);
            continue;
        }
        match replayed_text(&entry, live)? {
            Some(text) => {
                replayed.push(format!("/{}", entry.path));
                entry.content = Some(text.into_bytes());
                entry.base_revision = current_revision;
                merged.push(entry);
            }
            None => conflicts.push(ConflictFile {
                path: format!("/{}", entry.path),
                base_revision: entry.base_revision,
                current_revision,
            }),
        }
    }
    if conflicts.is_empty() {
        Ok(Merged {
            entries: merged,
            replayed,
        })
    } else {
        Err(OkfError::Conflict { files: conflicts })
    }
}

/// A stale entry's content redone on the current text.
///
/// # Returns
/// `None` when it cannot be replayed: a conflict. That is a deletion, a
/// free edit, unreadable operations, current content that is not text, or
/// a path deleted since the entry's base (the edit was made on a document
/// that is gone, so replaying would silently re-create it).
///
/// # Errors
/// The replay's own error (text it cannot place a bullet in).
fn replayed_text(entry: &EntryRow, live: Option<&CurrentRow>) -> Result<Option<String>, OkfError> {
    // A deletion is never replayed: whatever was published since is
    // something the changeset did not see.
    let (Some(_), Some(stored)) = (&entry.content, &entry.replay) else {
        return Ok(None);
    };
    // `write::apply` stored these; anything unreadable conflicts instead.
    let Ok(ops) = serde_json::from_value::<Vec<ReplayOp>>(stored.clone()) else {
        return Ok(None);
    };
    let current = match (live, entry.base_revision) {
        // A new path (no base) that is still not live: replay onto nothing.
        (None, None) => None,
        // Deleted since the entry's base: a conflict, not a re-creation.
        (None, Some(_)) => return Ok(None),
        (Some(row), _) => match std::str::from_utf8(&row.content) {
            Ok(text) => Some(text),
            Err(_) => return Ok(None),
        },
    };
    replay(&entry.path, current, &ops).map(Some)
}

/// The checks on the post-replay result (design "Publish", step 4).
///
/// # Returns
/// The overlay to write from (the current snapshot plus the entries) and
/// the lint advice.
///
/// # Errors
/// A content rule's error; an overlay that does not apply;
/// [`OkfError::LintFailed`].
async fn check(
    base: Arc<Loaded>,
    changeset: &ChangesetRow,
    entries: Vec<EntryRow>,
    now: DateTime<Utc>,
) -> Result<(Overlay, Vec<Finding>), OkfError> {
    let changed: BTreeSet<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let deleted: BTreeSet<String> = entries
        .iter()
        .filter(|entry| entry.content.is_none())
        .map(|entry| entry.path.clone())
        .collect();
    // The secret scan is CPU work; the entries go to the blocking thread and back.
    let entries = blocking(move || check_contents(entries)).await?;
    let overlay = build_overlay(base, changeset, entries).await?;
    let (overlay, findings) = blocking(move || {
        let findings = split_lint(&overlay, &changed, &deleted, now);
        (overlay, findings)
    })
    .await;
    if findings.errors.is_empty() {
        Ok((overlay, findings.warnings))
    } else {
        Err(OkfError::LintFailed {
            errors: findings.errors,
            warnings: findings.warnings,
        })
    }
}

/// The content rules on every changed text file. Write already checked
/// each entry, but publish checks the result again: replayed text is new,
/// and an entry may predate a rule.
///
/// # Errors
/// The first refused file's error, in path order (so a refusal is the same
/// on every attempt).
fn check_contents(entries: Vec<EntryRow>) -> Result<Vec<EntryRow>, OkfError> {
    for entry in &entries {
        // A file that is not text is stored and exported but not scanned
        // (design "Storage"); a deletion has no content.
        let text = entry
            .content
            .as_deref()
            .and_then(|content| std::str::from_utf8(content).ok());
        if let Some(text) = text {
            rules::check_content(&entry.path, text)?;
        }
    }
    Ok(entries)
}

/// Lint of the overlay, split for publish. Errors in changed files refuse,
/// as do links from unchanged documents into deleted paths. Errors
/// elsewhere become advice, beside the warnings in changed files. Warnings
/// elsewhere are left out: they predate this publish.
fn split_lint(
    overlay: &Overlay,
    changed: &BTreeSet<String>,
    deleted: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> LintReport {
    let report = overlay.lint(now);
    // `Finding::file` has no leading `/`.
    let in_changed = |finding: &Finding| changed.contains(&finding.file);
    // `partition` splits one list in two by the predicate, in one pass.
    let (mut errors, mut warnings): (Vec<Finding>, Vec<Finding>) =
        report.errors.into_iter().partition(in_changed);
    errors.extend(links_into_deleted(overlay.snapshot(), changed, deleted));
    warnings.extend(report.warnings.into_iter().filter(in_changed));
    LintReport { errors, warnings }
}

/// Links from documents this publish does not change into paths it deletes
/// (design "Publish", step 4). Changed documents' links into them are
/// already `broken-link` errors in changed files.
fn links_into_deleted(
    snapshot: &Snapshot,
    changed: &BTreeSet<String>,
    deleted: &BTreeSet<String>,
) -> Vec<Finding> {
    if deleted.is_empty() {
        return Vec::new();
    }
    snapshot
        .docs()
        .iter()
        .filter(|doc| doc.in_link_graph() && !changed.contains(doc.path()))
        // `flat_map` turns each document into its offending links, then
        // chains them; `move` gives the inner closure its own `doc` handle.
        .flat_map(|doc| {
            doc.links()
                .iter()
                .filter(|link| !link.in_page && deleted.contains(&link.target))
                .map(move |link| Finding {
                    file: doc.path().to_string(),
                    line: link.line,
                    rule: "link-into-deleted".to_string(),
                    message: format!("links to /{}, which this publish deletes", link.target),
                })
        })
        .collect()
}

/// Writes every changed path from the overlay (design "Publish", step 5).
///
/// # Returns
/// The revisions written, in path order. A path whose result equals the
/// published file gets no revision.
///
/// # Errors
/// [`OkfError::Database`]; a unique violation (two writers of one revision,
/// which the lock rules out) as [`OkfError::Conflict`].
async fn write_all(
    conn: &mut PgConnection,
    publication: &Publication<'_>,
    overlay: &Overlay,
    current: &HashMap<String, CurrentRow>,
    paths: &[String],
) -> Result<Vec<PublishedRevision>, OkfError> {
    let changeset = publication.changeset;
    let mut revisions = Vec::with_capacity(paths.len());
    for path in paths {
        let row = current.get(path);
        let live = row.filter(|row| row.live);
        let file = overlay.workspace().stored(path);
        let op = match (&file, live) {
            (Some(file), Some(row))
                if row.content == file.content && row.executable == file.executable =>
            {
                continue
            }
            (Some(_), Some(_)) => RevisionOp::Update,
            (Some(_), None) => RevisionOp::Create,
            (None, Some(_)) => RevisionOp::Delete,
            // A deletion of a path that has no live document: nothing to do.
            (None, None) => continue,
        };
        // A tombstone keeps its number, so a path created again continues it.
        let revision = row.map_or(1, |row| row.revision + 1);
        let write = DocumentWrite {
            catalog_id: changeset.catalog_id,
            path,
            revision,
            actor: publication.actor,
            now: publication.now,
        };
        let record = RevisionRecord {
            op,
            file: file.as_ref(),
            changeset_id: Some(changeset.id),
            message: publication.message,
        };
        let backstop = |error| as_conflict(error, path, live.map(|row| row.revision));
        match &file {
            Some(file) => {
                let derived = Derived::of(overlay.snapshot().get(path));
                documents::write_document(conn, &write, file, &derived)
                    .await
                    .map_err(backstop)?;
            }
            None => documents::delete_document(conn, &write)
                .await
                .map_err(backstop)?,
        }
        documents::record_revision(conn, &write, &record)
            .await
            .map_err(backstop)?;
        revisions.push(PublishedRevision {
            path: format!("/{path}"),
            revision,
            op: op.as_str(),
        });
    }
    Ok(revisions)
}

/// The design's backstop: a unique violation while writing is a conflict,
/// never the opaque `Database` error.
fn as_conflict(error: OkfError, path: &str, expected: Option<i32>) -> OkfError {
    match error {
        // `if` guard: this arm matches only a unique violation.
        OkfError::Database { source } if is_unique_violation(&source) => OkfError::Conflict {
            files: vec![ConflictFile {
                path: format!("/{path}"),
                base_revision: expected,
                // Unknown here: `publish` re-reads it after the rollback
                // (`with_current_revisions`).
                current_revision: None,
            }],
        },
        other => other,
    }
}

/// Fills in each conflicting file's live revision as committed now, for the
/// backstop's conflict (read on the pool, after the publish rolled back).
///
/// # Errors
/// [`OkfError::Database`].
async fn with_current_revisions(
    pool: &PgPool,
    catalog_id: i64,
    mut files: Vec<ConflictFile>,
) -> Result<Vec<ConflictFile>, OkfError> {
    // `&mut files`: the loop edits each file in place (one for the backstop).
    for file in &mut files {
        file.current_revision = sqlx::query_scalar(
            "SELECT revision FROM documents \
             WHERE catalog_id = $1 AND path = $2 AND deleted_at IS NULL",
        )
        .bind(catalog_id)
        .bind(file.path.trim_start_matches('/'))
        .fetch_optional(pool)
        .await?;
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, base: Option<i32>, text: Option<&str>) -> EntryRow {
        EntryRow {
            path: path.to_string(),
            base_revision: base,
            content: text.map(|text| text.as_bytes().to_vec()),
            executable: false,
            replay: None,
        }
    }

    fn row(text: &str, revision: i32, live: bool) -> CurrentRow {
        CurrentRow {
            content: text.as_bytes().to_vec(),
            executable: false,
            revision,
            live,
        }
    }

    #[test]
    fn a_deletion_published_since_the_first_edit_conflicts_even_with_a_replay() {
        // `base_revision: None` on a deletion: the path was published after
        // the changeset's first edit (write::apply stores no other kind).
        let mut deletion = entry("log.md", None, None);
        deletion.replay = Some(serde_json::json!([]));
        let current = HashMap::from([("log.md".to_string(), row("# Log\n", 1, true))]);

        let error = merge(vec![deletion], &current).unwrap_err();

        let OkfError::Conflict { files } = error else {
            panic!("not a conflict: {error:?}")
        };
        assert_eq!(
            files,
            [ConflictFile {
                path: "/log.md".to_string(),
                base_revision: None,
                current_revision: Some(1),
            }]
        );
    }

    #[test]
    fn a_replay_onto_a_path_deleted_since_its_base_conflicts() {
        // A `log` entry made on log.md revision 1; log.md is a tombstone now.
        let mut logged = entry("log.md", Some(1), Some("# Log\n\n## 2026-11-03\n\n* x\n"));
        let op = ReplayOp::Log {
            date: "2026-11-03".to_string(),
            bullet: "* x".to_string(),
        };
        logged.replay = Some(serde_json::json!([op]));
        let current = HashMap::from([("log.md".to_string(), row("", 2, false))]);

        let error = merge(vec![logged], &current).unwrap_err();

        let OkfError::Conflict { files } = error else {
            panic!("not a conflict: {error:?}")
        };
        assert_eq!(
            files,
            [ConflictFile {
                path: "/log.md".to_string(),
                base_revision: Some(1),
                current_revision: None,
            }]
        );
    }

    #[test]
    fn a_tombstone_counts_as_no_live_document() {
        // Base None, and the path is only a tombstone now: still fresh.
        let created = entry("a.md", None, Some("x"));
        let current = HashMap::from([("a.md".to_string(), row("", 4, false))]);

        let merged = merge(vec![created], &current).unwrap();

        assert_eq!(merged.entries.len(), 1);
        assert!(merged.replayed.is_empty());
    }

    #[test]
    fn as_conflict_passes_other_database_errors_through() {
        let error = as_conflict(
            OkfError::Database {
                source: sqlx::Error::PoolTimedOut,
            },
            "a.md",
            Some(1),
        );

        assert_eq!(error.kind(), "Database");
    }
}
