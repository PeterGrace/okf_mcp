//! The changeset tools (design "Write tools"):
//! - `publish` (Task 10);
//! - `discard`, `revert` and `pending_changes` (Task 11);
//! - `stranded_sessions` and `adopt_changeset` (Task 12).
//!
//! Each task's tools are one section with its own `#[tool_router]` block and
//! test module, so the tasks merge side by side; the shared lookup
//! ([`own_changeset`]) is at the top.

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::store::changesets::{self, ChangesetKey, ChangesetRow};
use crate::store::keys::Access;
// `as publishing`: the module's `publish` would clash with the tool's below.
use crate::store::publish::{self as publishing, PublishReport};

/// The catalog a changeset tool acts on, and this connection's open
/// changeset there (if any).
#[derive(Debug)]
struct OwnChangeset {
    state: Arc<CatalogState>,
    changeset: Option<ChangesetRow>,
}

/// Resolves a changeset tool's catalog, checks the key's access to it and
/// finds this connection's (key and label) open changeset.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `catalog` - the call's `catalog` argument; else the write target.
/// * `access` - the access the tool needs on that catalog.
///
/// # Returns
/// The catalog's state and the open changeset, if any.
///
/// # Errors
/// A selection error; [`OkfError::Forbidden`] below `access`;
/// [`OkfError::Database`].
async fn own_changeset(
    ctx: &ToolContext,
    catalog: Option<&str>,
    access: Access,
) -> Result<OwnChangeset, OkfError> {
    let state = ctx.one(catalog)?;
    let principal = ctx.principal();
    principal.require(access, state.name())?;
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &principal.key_id,
        label: ctx.label(),
    };
    let changeset = changesets::find_open(ctx.app().pool(), &key).await?;
    Ok(OwnChangeset { state, changeset })
}

// ---------------------------------------------------------------------------
// publish (Task 10)
// ---------------------------------------------------------------------------

/// `publish`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct PublishParams {
    /// What the change is, recorded on every revision it writes (like a commit message).
    pub message: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `publish`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Published {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The changeset published, if this connection had one open.
    pub changeset: Option<i64>,
    /// What was written; `flatten` puts its fields beside these.
    #[serde(flatten)]
    pub report: PublishReport,
}

/// Publishes this connection's changeset in the target catalog.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the message and catalog.
///
/// # Returns
/// The report; `published: false` without an open, non-empty changeset.
///
/// # Errors
/// [`OkfError::Refused`] for a blank message; as [`own_changeset`] (with
/// `write`); as [`publishing::publish`].
pub(crate) async fn publish(
    ctx: &ToolContext,
    params: PublishParams,
) -> Result<Published, OkfError> {
    let message = params.message.trim();
    if message.is_empty() {
        return Err(OkfError::Refused {
            reason: "publish needs a message saying what changed".to_string(),
        });
    }
    let OwnChangeset { state, changeset } =
        own_changeset(ctx, params.catalog.as_deref(), Access::Write).await?;
    let report = match &changeset {
        None => PublishReport::default(),
        Some(row) => {
            let actor = &ctx.principal().actor;
            publishing::publish(ctx.app(), &state, row, actor, message).await?
        }
    };
    Ok(Published {
        catalog: state.name().to_string(),
        label: ctx.label().to_string(),
        changeset: changeset.map(|row| row.id),
        report,
    })
}

#[tool_router(router = publish_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `publish` tool.
    #[tool(
        description = "Publish this connection's changeset in a catalog: its edits become the \
                       published documents, each with a new revision carrying `message`. Refused \
                       with Conflict when a document changed since this changeset read it \
                       (log.md and index.md bookkeeping is replayed onto the new text instead), \
                       or with LintFailed when a changed file has lint errors; the changeset \
                       then stays open. Lint errors elsewhere come back as warnings. Write access."
    )]
    async fn publish(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<PublishParams>,
    ) -> Result<Json<Published>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(publish(&ctx, params).await)
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use std::time::Duration;

    use chrono::Utc;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{app_with, context, files, principal_as, writer, TestApp};
    use crate::mcp::writes::{replace_body, ReplaceBodyParams};
    use crate::okf::ops::{self, LogKind};
    use crate::store::changesets::{ChangesetState, EntryRow};
    use crate::store::import::{import, ImportOptions};
    use crate::store::publish::PublishedRevision;
    use crate::store::{catalogs, history};
    use crate::write::{apply, WriteReport};

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const X: &str = "---\ntype: Note\ntitle: X\ndescription: X.\n---\n# X\n";
    const Y: &str = "---\ntype: Note\ntitle: Y\ndescription: Y.\n---\n# Y\n";
    const HUB: &str =
        "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\nSee [X](/infra/x.md).\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[
            ("a.md", NOTE),
            ("hub.md", HUB),
            ("infra/index.md", "# Infra\n"),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("log.md", "# Log\n"),
        ];
        app_with(&[("pg", files)], Config::default()).await
    }

    async fn body(ctx: &ToolContext, path: &str, text: &str) -> WriteReport {
        let params = ReplaceBodyParams {
            path: path.to_string(),
            body: text.to_string(),
            catalog: None,
        };
        replace_body(ctx, params).await.unwrap()
    }

    /// A `log` write, through the write path as Task 9's tool makes it.
    async fn log_entry(ctx: &ToolContext, text: &str) -> WriteReport {
        let text = text.to_string();
        apply(ctx, None, None, move |workspace, op| {
            ops::log(workspace, op, LogKind::Update, &text)
        })
        .await
        .unwrap()
    }

    /// An `index` write, through the write path as Task 9's tool makes it.
    async fn list(ctx: &ToolContext, path: &str, summary: &str) -> WriteReport {
        let (path, summary) = (path.to_string(), summary.to_string());
        apply(ctx, None, None, move |workspace, op| {
            ops::index(workspace, op, &path, &summary, None, None)
        })
        .await
        .unwrap()
    }

    async fn publish_as(ctx: &ToolContext, message: &str) -> Result<Published, OkfError> {
        let params = PublishParams {
            message: message.to_string(),
            catalog: None,
        };
        publish(ctx, params).await
    }

    fn published_text(test: &TestApp, path: &str) -> String {
        let loaded = test.app.catalog("pg").unwrap().current();
        loaded
            .snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    async fn state_of(test: &TestApp, changeset: i64) -> ChangesetState {
        changesets::by_id(test.app.pool(), changeset)
            .await
            .unwrap()
            .unwrap()
            .state
    }

    /// Stores an entry directly, as no write tool would (a rule changed, a
    /// hand edit), to show publish checks again.
    async fn insert_entry(test: &TestApp, key_id: &str, entry: EntryRow) {
        let state = test.app.catalog("pg").unwrap();
        let mut tx = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut tx, state.id()).await.unwrap();
        let key = ChangesetKey {
            catalog_id: state.id(),
            key_id,
            label: "default",
        };
        let row = changesets::get_or_create(&mut tx, &key, key_id, Utc::now())
            .await
            .unwrap();
        changesets::upsert_entry(&mut tx, row.id, &entry, Utc::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    /// Waits until `count` backends of this database wait on a lock.
    async fn lock_waiters(test: &TestApp, count: i64) {
        for _ in 0..500 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(test.app.pool())
            .await
            .unwrap();
            if waiting >= count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("{count} lock waiter(s) never appeared");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publish_writes_revisions_bumps_the_generation_and_swaps_the_snapshot() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        let before = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;
        let written = body(&ctx, "/a.md", "# A\n\nNew.\n").await;

        let published = publish_as(&ctx, "edit a").await.unwrap();

        assert!(published.report.published);
        assert_eq!(published.changeset, Some(written.changeset));
        assert_eq!(
            published.report.revisions,
            [PublishedRevision {
                path: "/a.md".to_string(),
                revision: 2,
                op: "update"
            }]
        );
        assert!(published.report.replayed.is_empty());
        let after = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;
        assert_eq!(after, before + 1);
        assert_eq!(state.current().generation(), after);
        assert!(published_text(&test, "a.md").ends_with("# A\n\nNew.\n"));
        let rows = history::revisions(test.app.pool(), state.id(), "a.md", 5)
            .await
            .unwrap();
        assert_eq!(
            (
                rows[0].revision,
                rows[0].op.as_str(),
                rows[0].actor.as_str(),
                rows[0].message.as_str()
            ),
            (2, "update", "writer-a", "edit a")
        );
        let changeset_id: Option<i64> = sqlx::query_scalar(
            "SELECT changeset_id FROM revisions WHERE path = 'a.md' AND revision = 2",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();
        assert_eq!(changeset_id, Some(written.changeset));
        let updated_by: String =
            sqlx::query_scalar("SELECT updated_by FROM documents WHERE path = 'a.md'")
                .fetch_one(test.app.pool())
                .await
                .unwrap();
        assert_eq!(updated_by, "writer-a");
        assert_eq!(
            state_of(&test, written.changeset).await,
            ChangesetState::Published
        );
        // With no open changeset left, this connection reads the published state.
        assert!(!ctx.view(&state).await.unwrap().pending());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_to_publish_is_a_result_and_a_message_is_required() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let before = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;

        let nothing = publish_as(&ctx, "nothing").await.unwrap();
        let blank = publish_as(&ctx, "  ").await.unwrap_err();

        assert!(!nothing.report.published);
        assert_eq!(nothing.changeset, None);
        assert_eq!(blank.kind(), "Refused");
        let after = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;
        assert_eq!(after, before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_free_edit_conflicts_and_leaves_the_changeset_open() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        body(&first, "/a.md", "From a.\n").await;
        let mine = body(&second, "/a.md", "From b.\n").await;
        publish_as(&first, "a first").await.unwrap();

        let error = publish_as(&second, "b second").await.unwrap_err();

        assert!(matches!(
            error,
            OkfError::Conflict { ref files } if files == &[ConflictFile {
                path: "/a.md".to_string(),
                base_revision: Some(1),
                current_revision: Some(2),
            }]
        ));
        assert_eq!(state_of(&test, mine.changeset).await, ChangesetState::Open);
        let entries = changesets::entries(test.app.pool(), mine.changeset)
            .await
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert!(published_text(&test, "a.md").ends_with("From a.\n"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_log_entries_both_publish_and_keep_both_bullets() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        log_entry(&first, "from a").await;
        log_entry(&second, "from b").await;

        // Both at once: the publish lock runs them one after the other, and
        // the second replays its entry onto the first's log.md.
        let (one, two) = tokio::join!(publish_as(&first, "a"), publish_as(&second, "b"));

        let (one, two) = (one.unwrap(), two.unwrap());
        assert!(one.report.published && two.report.published);
        let replayed: Vec<&String> = one
            .report
            .replayed
            .iter()
            .chain(&two.report.replayed)
            .collect();
        assert_eq!(replayed, ["/log.md"]);
        let text = published_text(&test, "log.md");
        assert!(
            text.contains("* **Update:** from a") && text.contains("* **Update:** from b"),
            "{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_index_listing_is_replayed() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        list(&first, "/infra/x.md", "the x").await;
        list(&second, "/infra/y.md", "the y").await;
        publish_as(&first, "list x").await.unwrap();

        let published = publish_as(&second, "list y").await.unwrap();

        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        let text = published_text(&test, "infra/index.md");
        assert!(
            text.contains("[X](x.md) - the x") && text.contains("[Y](y.md) - the y"),
            "{text}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lint_error_in_a_changed_file_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let written = body(&ctx, "/a.md", "See [gone](/gone.md).\n").await;

        let error = publish_as(&ctx, "broken").await.unwrap_err();

        // `&error`: the pattern borrows, so the `else` branch can still print it.
        let OkfError::LintFailed { errors, .. } = &error else {
            panic!("not LintFailed: {error:?}")
        };
        assert!(errors
            .iter()
            .any(|finding| finding.file == "a.md" && finding.rule == "broken-link"));
        assert_eq!(
            state_of(&test, written.changeset).await,
            ChangesetState::Open
        );
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deletion_still_linked_from_elsewhere_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let deletion = EntryRow {
            path: "infra/x.md".to_string(),
            base_revision: Some(1),
            content: None,
            executable: false,
            replay: None,
        };
        insert_entry(&test, "writer-a", deletion).await;

        let error = publish_as(&ctx, "drop x").await.unwrap_err();

        let OkfError::LintFailed { errors, .. } = &error else {
            panic!("not LintFailed: {error:?}")
        };
        assert!(errors
            .iter()
            .any(|finding| finding.file == "hub.md" && finding.rule == "link-into-deleted"));
        assert_eq!(published_text(&test, "infra/x.md"), X);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_checks_against_an_import_the_snapshot_has_not_seen() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let deletion = EntryRow {
            path: "infra/y.md".to_string(),
            base_revision: Some(1),
            content: None,
            executable: false,
            replay: None,
        };
        insert_entry(&test, "writer-a", deletion).await;
        // A CLI import (another process) adds a link to infra/y.md; this
        // server's snapshot has not been refreshed since.
        let linker = "---\ntype: Note\ntitle: L\ndescription: L.\n---\nSee [Y](/infra/y.md).\n";
        let incoming = [
            ("a.md", NOTE),
            ("hub.md", HUB),
            ("infra/index.md", "# Infra\n"),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("linker.md", linker),
            ("log.md", "# Log\n"),
        ];
        let options = ImportOptions {
            replace: true,
            actor: "cli",
            now: Utc::now(),
        };
        import(test.app.pool(), "pg", files(&incoming), &options)
            .await
            .unwrap();
        let state = test.app.catalog("pg").unwrap();
        let stale = state.current().generation();

        let error = publish_as(&ctx, "drop y").await.unwrap_err();

        // Step 2 rebuilt the snapshot first, so the new link was seen.
        let OkfError::LintFailed { errors, .. } = &error else {
            panic!("not LintFailed: {error:?}")
        };
        assert!(errors
            .iter()
            .any(|finding| finding.file == "linker.md" && finding.rule == "link-into-deleted"));
        assert_eq!(state.current().generation(), stale + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_secret_is_refused_at_publish_too() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        // Built at run time, so no source file holds a secret-shaped string.
        let leaked = format!("{NOTE}password = {}\n", "Ab1".repeat(4));
        let entry = EntryRow {
            path: "a.md".to_string(),
            base_revision: Some(1),
            content: Some(leaked.into_bytes()),
            executable: false,
            replay: None,
        };
        insert_entry(&test, "writer-a", entry).await;

        let error = publish_as(&ctx, "leak").await.unwrap_err();

        assert_eq!(error.kind(), "SecretDetected");
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_queued_behind_a_publish_lands_in_a_new_changeset() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        let opened = body(&ctx, "/a.md", "# A\n\nFirst.\n").await;
        // Hold the changeset's row lock, so the publish and then the write
        // queue on it in that order.
        let mut holder = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut holder, state.id())
            .await
            .unwrap();
        changesets::lock(&mut holder, opened.changeset)
            .await
            .unwrap();
        // Two connections of one key and label, each with its own context
        // (`ToolContext` is not `Clone`); `move` gives each task its own.
        let same_key = || {
            context(
                &test,
                principal_as("writer-a", Access::Write, &[], Some("pg")),
                None,
            )
        };
        let publisher = same_key();
        let publishing = tokio::spawn(async move { publish_as(&publisher, "first").await });
        lock_waiters(&test, 1).await;
        let late = same_key();
        let writing =
            tokio::spawn(async move { body(&late, "/infra/y.md", "# Y\n\nLate.\n").await });
        lock_waiters(&test, 2).await;

        holder.rollback().await.unwrap();
        let published = publishing.await.unwrap().unwrap();
        let written = writing.await.unwrap();

        // The publish took the lock first and closed the changeset. The
        // write's `get_or_create` re-checks `state = 'open'` once the lock
        // is granted, finds none, and opens a new changeset: never `Busy`,
        // and no edit lost.
        assert_eq!(published.changeset, Some(opened.changeset));
        assert_eq!(
            published.report.revisions,
            [PublishedRevision {
                path: "/a.md".to_string(),
                revision: 2,
                op: "update"
            }]
        );
        assert_ne!(written.changeset, opened.changeset);
        assert_eq!(
            state_of(&test, written.changeset).await,
            ChangesetState::Open
        );
        let entries = changesets::entries(test.app.pool(), written.changeset)
            .await
            .unwrap();
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["infra/y.md"]);
        assert_eq!(published_text(&test, "infra/y.md"), Y);
    }
}
