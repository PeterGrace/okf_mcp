//! The write path (design "Data flow", Write): the one function every write
//! tool calls.
//!
//! [`apply`] resolves the target catalog, requires `write` on it, and in one
//! transaction:
//! - takes the catalog row (`FOR KEY SHARE`, plan Decision 3);
//! - gets or creates the connection's changeset and locks its row;
//! - runs the edit on a copy of the connection's overlay, on a blocking
//!   thread, and checks the write rules on every path it touched;
//! - stores each touched path as a changeset entry.
//!
//! Then it commits and drops the cached overlay, so the connection's next
//! read sees the edit. A refused edit stores nothing.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;

use crate::error::OkfError;
use crate::mcp::blocking;
use crate::mcp::context::ToolContext;
use crate::okf::ops::replay::ReplayOp;
use crate::okf::ops::{OpContext, OpOutcome};
use crate::okf::rules;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::overlay::Overlay;
use crate::store::changesets::{self, ChangesetKey, ChangesetState, EntryRow};
use crate::store::keys::Access;

/// What a write did: the write tools' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct WriteReport {
    /// The catalog written to.
    pub catalog: String,
    /// The changeset holding the edit (what `adopt_changeset` takes).
    pub changeset: i64,
    /// The changeset's label (`X-OKF-Changeset`, `?changeset=`, or `default`).
    pub label: String,
    /// The `/`-prefixed paths that now have an entry in the changeset (written,
    /// or a deletion), in the op's order. A touched path that left nothing to
    /// store (gone, and never published) is not listed.
    pub touched: Vec<String>,
    /// What `okf.py` would have printed, e.g. `infra/x.md: generated { by: a, at: ... }`.
    pub message: String,
    /// `okf.py`'s notes (a non-kebab-case name, a folder index created).
    pub notes: Vec<String>,
}

/// One path the edit touched: its normalized name and what it holds now
/// (`None`: the edit removed it).
#[derive(Debug)]
struct Touched {
    path: String,
    file: Option<StoredFile>,
}

/// Applies an edit to the connection's changeset in the target catalog.
///
/// Writes to one changeset run one at a time: each holds the changeset's
/// row lock ([`changesets::lock`]) from before it reads the overlay until it
/// commits, so the overlay it edits holds every earlier write. The overlay's
/// cached `version` is never used as a concurrency token.
///
/// # Arguments
/// * `ctx` - the call's context: key, label, catalogs.
/// * `catalog` - the call's `catalog` argument; else the connection's write
///   target (the first selected catalog).
/// * `by` - the call's `by` argument; else the key's actor.
/// * `op` - the edit. It gets a copy of the overlay workspace (published
///   files plus this changeset's entries) and an [`OpContext`] with the
///   server clock's `now` and `by`, and returns what it touched. A touched
///   path that is not a file afterwards is stored as a deletion if it is
///   published (or was when the changeset first edited it); otherwise the
///   changeset keeps no entry for it.
///
/// # Returns
/// The changeset, the touched paths and the op's message and notes.
///
/// # Errors
/// - a selection error ([`OkfError::CatalogNotSelected`], [`OkfError::Refused`]);
/// - [`OkfError::Forbidden`] for a key below `write` on the catalog;
/// - [`OkfError::Refused`] (`CATALOG_GONE`) if the catalog was deleted while
///   this write waited;
/// - [`OkfError::Busy`] if the changeset was published or discarded while
///   this write waited for it (a retry opens a new one);
/// - an overlay that no longer applies ([`OkfError::InvalidPath`], naming the
///   changeset and how to recover);
/// - the op's own error;
/// - [`OkfError::InvalidPath`] for a touched path outside the catalog or in
///   machinery;
/// - [`OkfError::SecretDetected`] or [`OkfError::FrontmatterParse`] from the
///   content rules;
/// - [`OkfError::Database`].
// `F: FnOnce(...) + Send + 'static`: the edit is a closure called once, that
// may move to another thread (`Send`) and owns everything it uses
// (`'static`), because it runs on a blocking thread that may outlive this
// call's borrows. `&OpContext<'_>` lets it borrow `by` only during the call.
pub async fn apply<F>(
    ctx: &ToolContext,
    catalog: Option<&str>,
    by: Option<&str>,
    op: F,
) -> Result<WriteReport, OkfError>
where
    F: FnOnce(&MemWorkspace, &OpContext<'_>) -> Result<OpOutcome, OkfError> + Send + 'static,
{
    let state = ctx.one(catalog)?;
    let principal = ctx.principal();
    principal.require(Access::Write, state.name())?;
    let app = ctx.app();
    let now = app.clock().now();
    // Owned, so the blocking closure can take it.
    let by = by.unwrap_or(&principal.actor).to_string();
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &principal.key_id,
        label: ctx.label(),
    };

    // Dropping `tx` without `commit()` (any `?` below) rolls everything back:
    // a refused edit leaves no changeset and no entry behind.
    let mut tx = app.pool().begin().await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let opened = changesets::get_or_create(&mut tx, &key, &principal.actor, now).await?;
    let changeset = changesets::lock(&mut tx, opened.id)
        .await?
        // `filter` keeps the row only while it is still open: a publish or
        // discard that held the lock may have closed it.
        .filter(|row| row.state == ChangesetState::Open)
        .ok_or_else(|| OkfError::Busy {
            reason: format!(
                "changeset {} was published or discarded while this write waited",
                opened.id
            ),
        })?;
    // Read under the row lock, in this transaction (`&mut *tx` reborrows
    // it): no other write can change the entries meanwhile, so the overlay
    // holds every committed entry, and no second connection is needed.
    let overlay = app.overlays().get(&mut *tx, &state, &changeset).await?;
    let mut existing: HashMap<String, EntryRow> = changesets::entries(&mut *tx, changeset.id)
        .await?
        .into_iter()
        .map(|entry| (entry.path.clone(), entry))
        .collect();

    // `Arc::clone` gives the blocking thread its own handle on the overlay.
    let work = Arc::clone(&overlay);
    let (outcome, files) = blocking(move || run(&work, &by, now, op)).await?;

    // Only the paths that end up with an entry (written or a deletion).
    let mut paths = Vec::with_capacity(files.len());
    for Touched { path, file } in files {
        // `remove` takes the entry out of the map (no copy).
        let previous = existing.remove(&path);
        let published = overlay.base().revision(&path);
        let replay = outcome.replay.get(&path);
        match new_entry(previous.as_ref(), published, replay, path, file) {
            Ok(entry) => {
                changesets::upsert_entry(&mut tx, changeset.id, &entry, now).await?;
                paths.push(format!("/{}", entry.path));
            }
            // Nothing published and nothing left: drop the changeset's own
            // entry for it (a file it created, then removed), if any.
            Err(path) if previous.is_some() => {
                let paths = [path];
                changesets::remove_entries(&mut tx, changeset.id, Some(&paths), now).await?;
            }
            Err(_) => {}
        }
    }
    tx.commit().await?;
    // The version moved, so the cached overlay is stale; dropping it now
    // also frees its memory before the next read rebuilds it.
    app.overlays().forget(changeset.id);
    Ok(WriteReport {
        catalog: state.name().to_string(),
        changeset: changeset.id,
        label: changeset.label,
        touched: paths,
        message: outcome.message,
        notes: outcome.notes,
    })
}

/// The entry to store for one touched path.
///
/// A path that is gone after the edit, was not published when the
/// changeset first edited it, and is not published now has nothing for
/// publish to delete: no entry is stored for it. A deletion entry always
/// names something published (at its base revision or since), so publish
/// either deletes it or reports a conflict.
///
/// # Arguments
/// * `previous` - the path's entry before this edit, if any.
/// * `published` - the path's published revision now, if any.
/// * `new_ops` - this edit's replay operations for the path, if any.
/// * `path` - the normalized path.
/// * `file` - what the path holds now; `None` is a deletion.
///
/// # Returns
/// The entry: the first edit's base revision (else the published one), the
/// merged replay, and the content.
///
/// # Errors
/// `Err(path)`, handing the path back, when there is nothing to store.
// `Result<EntryRow, String>` here is "store this, or nothing for this path";
// it is not an error the caller reports.
fn new_entry(
    previous: Option<&EntryRow>,
    published: Option<i32>,
    new_ops: Option<&Vec<ReplayOp>>,
    path: String,
    file: Option<StoredFile>,
) -> Result<EntryRow, String> {
    // The first edit's base stays: publish compares it with the document's
    // revision then, whatever later edits saw.
    let base_revision = match previous {
        Some(entry) => entry.base_revision,
        None => published,
    };
    if file.is_none() && base_revision.is_none() && published.is_none() {
        return Err(path);
    }
    let replay = merged_replay(previous, new_ops);
    // `is_some_and` reads the `Option` without unwrapping it; the content
    // then moves out of `file` (no copy).
    let executable = file.as_ref().is_some_and(|file| file.executable);
    Ok(EntryRow {
        path,
        base_revision,
        content: file.map(|file| file.content),
        executable,
        replay,
    })
}

/// Runs the edit on a copy of the overlay and checks the rules on every
/// path it touched (on a blocking thread: the secret scan is CPU work).
///
/// # Arguments
/// * `overlay` - the connection's overlay, under the changeset's row lock.
/// * `by` - the actor for the op's `by:` fields.
/// * `now` - the server clock's time.
/// * `op` - the edit.
///
/// # Returns
/// The op's outcome and each touched path's normalized name and file.
///
/// # Errors
/// The overlay's copy failing (it does not, once built); the op's error; a
/// rule's error for the first refused path.
fn run<F>(
    overlay: &Overlay,
    by: &str,
    now: DateTime<Utc>,
    op: F,
) -> Result<(OpOutcome, Vec<Touched>), OkfError>
where
    F: FnOnce(&MemWorkspace, &OpContext<'_>) -> Result<OpOutcome, OkfError>,
{
    // A copy (Task 7's `with_changes` with no changes): the cached overlay
    // is shared with readers and never changes.
    let workspace = overlay.workspace().with_changes(std::iter::empty())?;
    let outcome = op(&workspace, &OpContext { now, by })?;
    // `collect` into `Result<Vec<_>, _>` stops at the first refusal.
    let files = outcome
        .touched
        .iter()
        .map(|path| touched_file(&workspace, path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((outcome, files))
}

/// Checks one touched path: inside the catalog and not machinery, and, if
/// it still exists, its content passes the secret scan and (for markdown)
/// its frontmatter parses.
///
/// # Arguments
/// * `workspace` - the edited copy.
/// * `path` - a path the op reported as touched.
///
/// # Returns
/// The normalized path and its file (`None`: no longer a file).
///
/// # Errors
/// [`OkfError::InvalidPath`], [`OkfError::SecretDetected`],
/// [`OkfError::FrontmatterParse`]; [`OkfError::Refused`] for content that is
/// not UTF-8 (no op writes any).
fn touched_file(workspace: &MemWorkspace, path: &str) -> Result<Touched, OkfError> {
    let path = rules::check_path(path)?;
    let file = workspace.stored(&path);
    if let Some(stored) = &file {
        let text = std::str::from_utf8(&stored.content).map_err(|_| OkfError::Refused {
            reason: format!("{path}: the write tools only write UTF-8 text"),
        })?;
        rules::check_content(&path, text)?;
    }
    Ok(Touched { path, file })
}

/// The `replay` an entry stores after one more edit (plan Interfaces, Task 8).
///
/// While every edit to an entry came from bookkeeping, its replay is their
/// operations in order, so publish can redo them on newer text. One free
/// edit (no operations for this path) clears it for good: publish then
/// treats the entry like any other edit.
///
/// # Arguments
/// * `previous` - the path's entry before this edit, if any.
/// * `new_ops` - this edit's operations for the path, if it was bookkeeping.
///
/// # Returns
/// The JSON array to store, or `None` (a free edit).
fn merged_replay(
    previous: Option<&EntryRow>,
    new_ops: Option<&Vec<ReplayOp>>,
) -> Option<serde_json::Value> {
    // `?` on an `Option` returns `None` early, like `if x is None: return None`.
    let new_ops = new_ops?;
    let mut ops = match previous {
        None => Vec::with_capacity(new_ops.len()),
        // Stored by this function, so always an array; anything else (a
        // hand edit) is treated as a free edit, which is the safe side.
        Some(entry) => match &entry.replay {
            Some(serde_json::Value::Array(stored)) => {
                let mut ops = Vec::with_capacity(stored.len() + new_ops.len());
                ops.extend(stored.iter().cloned());
                ops
            }
            Some(_) | None => return None,
        },
    };
    ops.extend(new_ops.iter().map(|op| {
        serde_json::to_value(op).expect("a ReplayOp (strings and a bool) always serializes")
    }));
    Some(serde_json::Value::Array(ops))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, files, pending_entry, principal, principal_as, store_key, writer,
        PendingEdit, TestApp,
    };
    use crate::okf::workspace::Workspace;
    use crate::store::import::{import, ImportOptions};
    use std::collections::BTreeMap;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";

    async fn pg() -> TestApp {
        app_with(
            &[("pg", &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")])],
            Config::default(),
        )
        .await
    }

    /// An outcome that touched `paths` and recorded no replay.
    fn touched(paths: &[&str]) -> OpOutcome {
        OpOutcome {
            touched: paths.iter().map(|path| path.to_string()).collect(),
            replay: BTreeMap::new(),
            message: "test".to_string(),
            notes: Vec::new(),
        }
    }

    /// Writes `text` to `path`, as an op would.
    async fn write(
        ctx: &ToolContext,
        path: &'static str,
        text: String,
    ) -> Result<WriteReport, OkfError> {
        // `move` hands `text` to the closure, which `apply` runs on another thread.
        apply(ctx, None, None, move |ws, _| {
            ws.write_atomic(path, &text)?;
            Ok(touched(&[path]))
        })
        .await
    }

    async fn entries(test: &TestApp, changeset: i64) -> Vec<EntryRow> {
        changesets::entries(test.app.pool(), changeset)
            .await
            .unwrap()
    }

    /// A `SELECT count(*)` query's answer (sqlx 0.9 takes only literal SQL).
    async fn count(test: &TestApp, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(test.app.pool())
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_opens_a_changeset_and_stores_an_entry() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = write(&ctx, "a.md", format!("{NOTE}More.\n")).await.unwrap();

        assert_eq!(
            (report.catalog.as_str(), report.label.as_str()),
            ("pg", "default")
        );
        assert_eq!(report.touched, ["/a.md"]);
        let row = changesets::by_id(test.app.pool(), report.changeset)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.state, ChangesetState::Open);
        assert_eq!(
            (row.key_id.as_deref(), row.actor.as_str()),
            (Some("writer-a"), "writer-a")
        );
        let stored = entries(&test, report.changeset).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].path, "a.md");
        assert_eq!(stored[0].base_revision, Some(1));
        assert_eq!(
            stored[0].content.as_deref(),
            Some(format!("{NOTE}More.\n").as_bytes())
        );
        assert_eq!(stored[0].replay, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_read_key_is_refused_before_anything_is_stored() {
        let test = pg().await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let error = write(&ctx, "a.md", "x".to_string()).await.unwrap_err();

        assert_eq!(error.kind(), "Forbidden");
        assert_eq!(count(&test, "SELECT count(*) FROM changesets").await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_write_keeps_the_first_base_revision() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let first = write(&ctx, "a.md", format!("{NOTE}One.\n")).await.unwrap();
        // Someone publishes a.md meanwhile (an import stands in for it), and
        // the server notices.
        let options = ImportOptions {
            replace: true,
            actor: "t",
            now: Utc::now(),
        };
        let republished = files(&[("a.md", "# Changed\n"), ("AGENTS.md", "# Agents\n")]);
        import(test.app.pool(), "pg", republished, &options)
            .await
            .unwrap();
        let state = test.app.catalog("pg").unwrap();
        state.refresh(test.app.pool()).await.unwrap();
        assert_eq!(state.current().revision("a.md"), Some(2));

        let second = write(&ctx, "a.md", format!("{NOTE}Two.\n")).await.unwrap();

        assert_eq!(second.changeset, first.changeset);
        let stored = entries(&test, second.changeset).await;
        // Still the revision the first edit was made on: publish must see
        // that the published document moved on since.
        assert_eq!(stored[0].base_revision, Some(1));
        assert_eq!(
            stored[0].content.as_deref(),
            Some(format!("{NOTE}Two.\n").as_bytes())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn machinery_and_secrets_are_refused_and_nothing_is_stored() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // Built at run time, so no source file holds a secret-shaped string.
        let secret = format!("{NOTE}password = {}\n", "Ab1".repeat(4));

        let machinery = write(&ctx, "AGENTS.md", "# Mine now\n".to_string())
            .await
            .unwrap_err();
        let leaked = write(&ctx, "a.md", secret).await.unwrap_err();

        assert_eq!(machinery.kind(), "InvalidPath");
        assert!(
            matches!(
                leaked,
                OkfError::SecretDetected { ref path, line: 7, ref rule, .. }
                    if path == "a.md" && rule == "secret:assignment"
            ),
            "{leaked:?}"
        );
        assert_eq!(
            count(&test, "SELECT count(*) FROM changeset_entries").await,
            0
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn by_defaults_to_the_keys_actor_and_now_comes_from_the_clock() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // A plain function (no captures), so it is `'static` as `apply` needs.
        fn record(ws: &MemWorkspace, op_ctx: &OpContext<'_>) -> Result<OpOutcome, OkfError> {
            ws.write_atomic("a.md", &format!("{} {}", op_ctx.by, op_ctx.now.timestamp()))?;
            Ok(touched(&["a.md"]))
        }

        let before = Utc::now().timestamp();
        let defaulted = apply(&ctx, None, None, record).await.unwrap();
        let content = entries(&test, defaulted.changeset).await[0]
            .content
            .clone()
            .unwrap();
        let text = String::from_utf8(content).unwrap();
        apply(&ctx, None, Some("human:pgrace"), record)
            .await
            .unwrap();
        let overridden = entries(&test, defaulted.changeset).await[0]
            .content
            .clone()
            .unwrap();

        let (by, at) = text.split_once(' ').unwrap();
        assert_eq!(by, "writer-a");
        assert!(at.parse::<i64>().unwrap() >= before);
        assert!(overridden.starts_with(b"human:pgrace "));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_overlay_shows_the_edit_to_this_key_and_label_only() {
        let test = pg().await;
        let author = writer(&test, "writer-a", "pg").await;
        let other_key = writer(&test, "writer-b", "pg").await;
        // The same key (stored above), on another label.
        let other_label = context(
            &test,
            principal_as("writer-a", Access::Write, &[], Some("pg")),
            None,
        )
        .with_label("draft");
        let state = test.app.catalog("pg").unwrap();
        // The view before the write, to show the write invalidates the cache.
        author.view(&state).await.unwrap();

        write(&author, "a.md", format!("{NOTE}Pending.\n"))
            .await
            .unwrap();

        let text =
            |view: crate::overlay::View| view.snapshot().get("a.md").unwrap().text().to_string();
        assert_eq!(
            text(author.view(&state).await.unwrap()),
            format!("{NOTE}Pending.\n")
        );
        assert_eq!(text(other_key.view(&state).await.unwrap()), NOTE);
        assert_eq!(text(other_label.view(&state).await.unwrap()), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_gone_path_is_a_deletion_only_if_something_is_published() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // Deletion entries as a removing op leaves them (Task 11's `revert`
        // removes files with `MemWorkspace::remove`, not added yet): one for
        // the published a.md, one for new.md, which was never published.
        for path in ["a.md", "new.md"] {
            let edit = PendingEdit {
                catalog: "pg",
                path,
                text: None,
                at: Utc::now(),
            };
            pending_entry(&test, &ctx, &edit).await;
        }

        // Every path is gone in the overlay; gone.md never had an entry.
        let report = apply(&ctx, None, None, |_, _| {
            Ok(touched(&["a.md", "new.md", "gone.md"]))
        })
        .await
        .unwrap();

        // Only the path with an entry is reported.
        assert_eq!(report.touched, ["/a.md"]);
        let stored = entries(&test, report.changeset).await;
        // Only a.md, published, is left to delete; the others are dropped.
        let paths: Vec<&str> = stored.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["a.md"]);
        assert_eq!(stored[0].content, None);
    }

    #[test]
    fn a_gone_published_path_keeps_its_first_base_revision() {
        let earlier = EntryRow {
            path: "a.md".to_string(),
            base_revision: Some(1),
            content: Some(b"edited".to_vec()),
            executable: false,
            replay: None,
        };

        let fresh = new_entry(None, Some(2), None, "a.md".to_string(), None).unwrap();
        let after_edit = new_entry(Some(&earlier), None, None, "a.md".to_string(), None).unwrap();
        let nothing = new_entry(None, None, None, "x.md".to_string(), None);

        assert_eq!((fresh.base_revision, fresh.content), (Some(2), None));
        // Unpublished since, but the base names what publish must check.
        assert_eq!(
            (after_edit.base_revision, after_edit.content),
            (Some(1), None)
        );
        assert_eq!(nothing, Err("x.md".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_writes_to_one_changeset_lose_no_edit() {
        let test = pg().await;
        // The contexts are built per task below, so only the key row here.
        store_key(&test, "writer-a").await;
        let lines: Vec<String> = (0..6).map(|n| format!("Line {n}.\n")).collect();

        // Each task appends its own line to whatever a.md holds when its op
        // runs. Without the changeset's row lock (`get_or_create` takes it,
        // `changesets::lock` holds it and re-reads the row) two of them would
        // read the same text, and the later upsert would drop the earlier
        // line. The first writes also race to open the changeset.
        let tasks: Vec<_> = lines
            .iter()
            .cloned()
            .map(|line| {
                let ctx = context(
                    &test,
                    principal_as("writer-a", Access::Write, &[], Some("pg")),
                    None,
                );
                tokio::spawn(async move {
                    apply(&ctx, None, None, move |ws, _| {
                        let text = ws.read_to_string("a.md")?;
                        ws.write_atomic("a.md", &format!("{text}{line}"))?;
                        Ok(touched(&["a.md"]))
                    })
                    .await
                })
            })
            .collect();
        let mut changeset_ids = Vec::with_capacity(tasks.len());
        for task in tasks {
            changeset_ids.push(task.await.unwrap().unwrap().changeset);
        }

        changeset_ids.dedup();
        assert_eq!(
            changeset_ids.len(),
            1,
            "one changeset for one key and label"
        );
        let content = entries(&test, changeset_ids[0]).await[0]
            .content
            .clone()
            .unwrap();
        let text = String::from_utf8(content).unwrap();
        for line in &lines {
            assert!(text.contains(line.as_str()), "{line:?} lost: {text}");
        }
    }

    #[test]
    fn replay_operations_accumulate_until_a_free_edit() {
        let log = |bullet: &str| ReplayOp::Log {
            date: "2026-11-03".to_string(),
            bullet: bullet.to_string(),
        };
        let entry = |replay: Option<serde_json::Value>| EntryRow {
            path: "log.md".to_string(),
            base_revision: Some(1),
            content: Some(Vec::new()),
            executable: false,
            replay,
        };
        let first = vec![log("* one")];
        let second = vec![log("* two")];

        let new = merged_replay(None, Some(&first));
        let appended = merged_replay(Some(&entry(new.clone())), Some(&second));
        let freed = merged_replay(Some(&entry(appended.clone())), None);
        let stays_free = merged_replay(Some(&entry(None)), Some(&second));
        let free_from_the_start = merged_replay(None, None);

        assert_eq!(
            new,
            Some(serde_json::json!([{"op": "log", "date": "2026-11-03", "bullet": "* one"}]))
        );
        assert_eq!(
            appended,
            Some(serde_json::json!([
                {"op": "log", "date": "2026-11-03", "bullet": "* one"},
                {"op": "log", "date": "2026-11-03", "bullet": "* two"}
            ]))
        );
        assert_eq!(freed, None);
        assert_eq!(stays_free, None);
        assert_eq!(free_from_the_start, None);
    }
}
