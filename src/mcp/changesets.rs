//! The changeset tools (design "Write tools"): `publish` (Task 10);
//! `discard`, `revert` and `pending_changes` (Task 11); `list_changesets`
//! (M8 Task 4, replacing Task 12's `stranded_sessions` and
//! `adopt_changeset`); `open_changeset` (M8 Task 2). Each task's tools are
//! one `#[tool_router]` block with its own router, joined in
//! `OkfServer::tools`.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::http::request::Parts;
use chrono::{DateTime, TimeDelta, Utc};
use rayon::prelude::*;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use similar::TextDiff;

use crate::catalog::Loaded;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::{Pinned, ToolContext};
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::ops::OpOutcome;
use crate::okf::paths::{escapes_root, normpath};
use crate::okf::rules;
use crate::okf::workspace::Workspace;
use crate::store::keys::Access;
use crate::write::{apply, WriteReport};

/// `list_changesets`' arguments.
// `Deserialize` reads them from the call's JSON; `JsonSchema` gives the tool
// its input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ListParams {
    /// Only this catalog (default: every catalog the connection offers: the
    /// key's, limited to the connection's list when it has one).
    pub catalog: Option<String>,
}

/// One open changeset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ListedChangeset {
    /// Its handle: pass it as `changeset` to resume work on it.
    pub changeset: String,
    /// Its catalog.
    pub catalog: String,
    /// The key it belongs to; null once that key is deleted.
    pub key: Option<String>,
    /// The key's grouping name; null once that key is deleted.
    pub key_name: Option<String>,
    /// The actor of the key that opened it.
    pub actor: String,
    /// When it was opened.
    pub created: String,
    /// Its last change.
    pub last_change: String,
    /// No change within `session_idle_timeout`.
    pub idle: bool,
    /// Its changed paths, `/`-prefixed and sorted.
    pub paths: Vec<String>,
}

/// `list_changesets`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Listed {
    /// By catalog, oldest change first.
    pub changesets: Vec<ListedChangeset>,
}

/// The time before which an open changeset's last change makes it idle.
///
/// # Arguments
/// * `now` - the current time (server clock).
/// * `idle` - `session_idle_timeout`.
///
/// # Returns
/// `now - idle`, or the earliest representable time if that would overflow
/// (an absurdly long timeout: nothing is idle).
fn idle_cutoff(now: DateTime<Utc>, idle: Duration) -> DateTime<Utc> {
    // `TimeDelta::from_std` fails for a duration beyond chrono's range, and
    // `checked_sub_signed` for a result before its earliest date; both `ok()`
    // and `and_then` turn a failure into `None`.
    TimeDelta::from_std(idle)
        .ok()
        .and_then(|idle| now.checked_sub_signed(idle))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Lists the open changesets this key may use (M8 Decision 8).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - an optional catalog to narrow the list to.
///
/// # Returns
/// Open changesets, empty ones included, in the catalogs the connection
/// offers ([`ToolContext::offers`]) or the one named: the key's own, and
/// with `admin` every key's.
///
/// # Errors
/// A selection error for `catalog`; [`OkfError::Database`].
pub(crate) async fn list_changesets(
    ctx: &ToolContext,
    params: ListParams,
) -> Result<Listed, OkfError> {
    // Local, as in the other tasks' functions here.
    use crate::store::changesets;

    let principal = ctx.principal();
    let catalog_ids: Vec<i64> = match params.catalog.as_deref() {
        // `one` refuses a catalog the key does not reach.
        Some(name) => vec![ctx.one(Some(name))?.id()],
        None => {
            // A list naming a bad catalog fails here as it does elsewhere.
            ctx.check_connection()?;
            ctx.catalogs()
                .values()
                .filter(|state| ctx.offers(state.name()))
                .map(|state| state.id())
                .collect()
        }
    };
    let admin = principal.access >= Access::Admin;
    // The database narrows by key (unless admin) and catalog, so other
    // keys' changesets and paths never leave it.
    let key_filter = (!admin).then_some(principal.key_id.as_str());
    let rows = changesets::list_open(ctx.app().pool(), &catalog_ids, key_filter).await?;
    let idle_since = idle_cutoff(
        ctx.app().clock().now(),
        ctx.app().config().session_idle_timeout,
    );
    let changesets = rows
        .into_iter()
        .map(|row| ListedChangeset {
            changeset: row.changeset.handle,
            catalog: row.catalog,
            key: row.changeset.key_id,
            key_name: row.key_name,
            actor: row.changeset.actor,
            created: format_timestamp(row.changeset.created_at),
            last_change: format_timestamp(row.changeset.updated_at),
            idle: row.changeset.updated_at < idle_since,
            paths: row.paths.iter().map(|path| format!("/{path}")).collect(),
        })
        .collect();
    Ok(Listed { changesets })
}

// As in the read tool modules: the macro turns this block's `#[tool]`
// methods into `lifecycle_router`, which `OkfServer::tools` joins.
#[tool_router(router = lifecycle_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `list_changesets` tool.
    #[tool(
        title = "List Changesets",
        description = "List the open changesets this key may use (an admin: every key's, in the \
                       catalogs it reaches), with their handles, paths and last change; idle \
                       means nothing changed within the idle timeout. Empty changesets are \
                       listed too, but one with no write within 24 hours is discarded and no \
                       longer listed: open a new one then. Use it to find a handle again after a \
                       reconnect or a lost context. Read access.",
        annotations(
            title = "List Changesets",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn list_changesets(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ListParams>,
    ) -> Result<Json<Listed>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(list_changesets(&ctx, params).await)
    }
}

// ---------------------------------------------------------------------------
// open_changeset (M8 Task 2)
// ---------------------------------------------------------------------------

/// `open_changeset`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct OpenParams {
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `open_changeset`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Opened {
    /// The new changeset's handle: pass it as `changeset` to every write, `publish`, `discard`, `revert` and `pending_changes`, and to reads that should see the drafts.
    pub changeset: String,
    /// Its catalog.
    pub catalog: String,
}

/// Opens a changeset and returns its handle (M8 Decision 1).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog; else the connection's write target.
///
/// # Returns
/// The new changeset's handle and its catalog's name.
///
/// # Errors
/// A selection error for `catalog`; [`OkfError::Forbidden`] without `write`;
/// [`OkfError::Refused`] if the catalog was deleted meanwhile;
/// [`OkfError::Busy`] on a deadlock; [`OkfError::Database`].
pub(crate) async fn open_changeset(
    ctx: &ToolContext,
    params: OpenParams,
) -> Result<Opened, OkfError> {
    // Local, as in the other tasks' functions here.
    use crate::store::changesets;

    let state = ctx.one(params.catalog.as_deref())?;
    let principal = ctx.principal();
    principal.require(Access::Write, state.name())?;
    let now = ctx.app().clock().now();
    // The lock order of every changeset write: the catalog row first, so a
    // concurrent `catalog delete` either waits for this or is seen here.
    let mut tx = ctx.app().pool().begin().await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    let opened = changesets::open(
        &mut tx,
        state.id(),
        &principal.key_id,
        &principal.actor,
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(Opened {
        changeset: opened.handle,
        catalog: state.name().to_string(),
    })
}

#[tool_router(router = open_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `open_changeset` tool.
    #[tool(
        title = "Open Changeset",
        description = "Open a changeset in a catalog and return its handle. Every edit, \
                       publish, discard, revert and pending_changes needs a handle, and the \
                       reads take one to see its drafts. Nothing is visible to anyone else until \
                       publish. Several changesets may be open at once. A changeset with no \
                       edits is discarded after 24 hours. Write access.",
        annotations(
            title = "Open Changeset",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn open_changeset(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<OpenParams>,
    ) -> Result<Json<Opened>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(open_changeset(&ctx, params).await)
    }
}

// ---------------------------------------------------------------------------
// publish (Task 10)
// ---------------------------------------------------------------------------

/// `publish`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct PublishParams {
    /// What the change is, recorded on every revision it writes (like a commit message).
    pub message: String,
    /// The changeset to publish: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `publish`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Published {
    /// The changeset's catalog.
    pub catalog: String,
    /// The changeset's handle. With `published: false` it was not published and stays open (it held no entries, for instance after a discard emptied it).
    pub changeset: String,
    /// What was written; `flatten` puts its fields beside these.
    #[serde(flatten)]
    pub report: crate::store::publish::PublishReport,
}

/// Publishes the changeset a handle names, in its catalog.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the message and the changeset.
///
/// # Returns
/// The report; `published: false` for a changeset with no entries.
///
/// # Errors
/// [`OkfError::Refused`] for a blank message; as [`ToolContext::changeset`]
/// (with `write`); as [`crate::store::publish::publish`].
pub(crate) async fn publish(
    ctx: &ToolContext,
    params: PublishParams,
) -> Result<Published, OkfError> {
    // `as publishing`: the module's `publish` would clash with this function.
    use crate::store::publish as publishing;

    let message = params.message.trim();
    if message.is_empty() {
        return Err(OkfError::Refused {
            reason: "publish needs a message saying what changed".to_string(),
        });
    }
    let Pinned { state, row } = ctx.changeset(&params.changeset, Access::Write).await?;
    let actor = &ctx.principal().actor;
    let report = publishing::publish(ctx.app(), &state, &row, actor, message).await?;
    Ok(Published {
        catalog: state.name().to_string(),
        changeset: row.handle,
        report,
    })
}

#[tool_router(router = publish_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `publish` tool.
    #[tool(
        title = "Publish Changeset",
        description = "Publish the changeset `changeset` names: its edits become the \
                       published documents, each with a new revision carrying `message`. Refused \
                       with Conflict when a document changed since this changeset read it \
                       (log.md and index.md bookkeeping is replayed onto the new text instead), \
                       or with LintFailed when a changed file has lint errors; the changeset \
                       then stays open. Lint errors elsewhere come back as warnings. When every \
                       edit already matches the published text, the changeset is closed with \
                       published: true and no revisions. Write access.",
        annotations(
            title = "Publish Changeset",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
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

// ---------------------------------------------------------------------------
// discard, revert and pending_changes (Task 11)
// ---------------------------------------------------------------------------

/// `discard`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DiscardParams {
    /// The paths to drop, e.g. `["/infra/ntp.md"]` (default: every entry, which also closes the changeset for good).
    pub paths: Option<Vec<String>>,
    /// The changeset to drop entries from: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `discard`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Discarded {
    /// The changeset's catalog.
    pub catalog: String,
    /// The changeset's handle.
    pub changeset: String,
    /// How many entries were dropped.
    pub removed: u64,
    /// Whether the changeset was closed (`discarded`): no `paths` given. Its handle is then refused; call `open_changeset` for new work.
    pub closed: bool,
}

/// `revert`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct RevertParams {
    /// The document as stored, with its extension, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The revision to go back to, as `history` lists it; a `delete` revision means deleting the document.
    pub revision: i32,
    /// The changeset to write to: the handle `open_changeset` returned.
    pub changeset: String,
}

/// One file of `pending_changes`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PendingFile {
    /// `/`-prefixed path.
    pub path: String,
    /// `create`, `update` or `delete`, against the published catalog now.
    pub change: &'static str,
    /// The published revision the entry was made on (null: the path had no live document).
    pub base_revision: Option<i32>,
    /// The path's published revision now (null: no live document).
    pub current_revision: Option<i32>,
    /// Whether the path was published since the entry was made (`base_revision` differs from `current_revision`). Publish replays a stale replayable entry and refuses any other with Conflict.
    pub stale: bool,
    /// Whether publish can replay the entry onto newer text (log.md and index.md bookkeeping only).
    pub replayable: bool,
    /// A unified diff against the published text (`/dev/null` for a new or deleted file). A deletion of a path deleted since (no published file), or a file equal to the published one, gets a one-line note instead.
    pub diff: String,
}

/// `pending_changes`' arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct PendingParams {
    /// The changeset to show: the handle `open_changeset` returned.
    pub changeset: String,
}

/// `pending_changes`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PendingChanges {
    /// The changeset's catalog.
    pub catalog: String,
    /// The changeset's handle.
    pub changeset: String,
    /// Its entries, in path order.
    pub files: Vec<PendingFile>,
}

/// A path argument as stored: no leading `/`, normalized, inside the
/// catalog. Unlike [`rules::check_path`] it allows machinery, so `discard`
/// can drop any entry, even one a later rule would refuse.
///
/// # Arguments
/// * `reference` - the path as the agent gave it.
///
/// # Returns
/// The stored form.
///
/// # Errors
/// [`OkfError::InvalidPath`] for a path outside the catalog or the root.
fn stored_path(reference: &str) -> Result<String, OkfError> {
    let path = normpath(reference.trim_start_matches('/'));
    if escapes_root(&path) || path == "." {
        return Err(OkfError::InvalidPath {
            path: reference.to_string(),
            reason: "not a path inside the catalog".to_string(),
        });
    }
    Ok(path)
}

/// Drops entries from the changeset a handle names; without `paths`, all
/// of them, and the changeset is closed (`discarded`) for good: its handle
/// is refused from then on.
///
/// Takes publish's locks in publish's order: the catalog's in-process
/// publish lock, then (in the transaction) the advisory lock, the catalog
/// row and the changeset row. So a discard and a publish of one changeset
/// run one after the other, and a write queued behind a closing discard is
/// refused.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the paths and the changeset.
///
/// # Returns
/// How many entries went, and whether the changeset was closed.
///
/// # Errors
/// [`OkfError::InvalidPath`] for a path outside the catalog; as
/// [`ToolContext::changeset`] (with `write`); [`OkfError::Refused`] if a
/// publish or another discard closed the changeset meanwhile;
/// [`OkfError::Database`].
pub(crate) async fn discard(
    ctx: &ToolContext,
    params: DiscardParams,
) -> Result<Discarded, OkfError> {
    use crate::store::catalogs;
    use crate::store::changesets::{self, ChangesetState};

    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>`, so
    // `?` can raise a bad path while "no paths" stays `None`.
    let paths = params
        .paths
        .map(|paths| {
            paths
                .iter()
                .map(|path| stored_path(path))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    let Pinned {
        state,
        row: changeset,
    } = ctx.changeset(&params.changeset, Access::Write).await?;
    let mut discarded = Discarded {
        catalog: state.name().to_string(),
        changeset: params.changeset,
        removed: 0,
        closed: false,
    };
    // An empty list drops nothing: no lock, no version bump, and the cached
    // overlay stays valid.
    if paths.as_ref().is_some_and(Vec::is_empty) {
        return Ok(discarded);
    }
    let app = ctx.app();
    // The guard releases the mutex when it goes out of scope, after the
    // overlay is forgotten.
    let _closing = state.publish_lock().lock().await;
    let now = app.clock().now();
    // Dropping `tx` without `commit()` (any `?`) rolls back.
    let mut tx = app.pool().begin().await?;
    catalogs::lock(&mut tx, state.id()).await?;
    changesets::lock_catalog(&mut tx, state.id()).await?;
    changesets::lock(&mut tx, changeset.id)
        .await?
        .filter(|row| row.state == ChangesetState::Open)
        .ok_or_else(|| OkfError::Refused {
            reason: format!(
                "changeset {} was published or discarded meanwhile",
                changeset.handle
            ),
        })?;
    discarded.removed =
        changesets::remove_entries(&mut tx, changeset.id, paths.as_deref(), now).await?;
    discarded.closed = paths.is_none();
    if discarded.closed {
        changesets::set_state(&mut tx, changeset.id, ChangesetState::Discarded, now).await?;
    }
    tx.commit().await?;
    app.overlays().forget(changeset.id);
    Ok(discarded)
}

/// Adds a document's content at `revision` (or its deletion, for a
/// `delete` revision) to the changeset a handle names, as a free edit.
///
/// It goes through [`apply`], so every write rule applies. Sending a path
/// the changeset created (never published) back to "absent" leaves no
/// entry for it, so the report's `touched` is empty.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the path, revision and changeset.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// [`OkfError::InvalidPath`] for machinery or a path outside the catalog
/// (before any history is read); as [`ToolContext::changeset`] (with
/// `write`); [`OkfError::NotFound`] for a
/// revision the path never had; [`OkfError::Refused`] for content that is
/// not UTF-8, or a deletion of a path this changeset's view does not hold;
/// as [`apply`].
pub(crate) async fn revert(
    ctx: &ToolContext,
    params: RevertParams,
) -> Result<WriteReport, OkfError> {
    use crate::store::history;

    let RevertParams {
        path,
        revision,
        changeset,
    } = params;
    // The write rules refuse machinery here, before the history is read;
    // `apply` checks the touched path again.
    let path = rules::check_path(&path)?;
    // The history is the changeset's catalog's; `apply` resolves the handle
    // again under its locks.
    let state = ctx.changeset(&changeset, Access::Write).await?.state;
    let stored = history::content_at(ctx.app().pool(), state.id(), &path, revision)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("/{path} (revision {revision})"),
        })?;
    // `transpose` again: `Option<Result<String, _>>` to `Result<Option<..>>`.
    let content = stored
        .content
        .map(String::from_utf8)
        .transpose()
        .map_err(|_| OkfError::Refused {
            reason: format!(
                "/{path} revision {revision} is not text; only text documents can be reverted"
            ),
        })?;
    // `move` hands `path` and `content` to the closure, which `apply` runs
    // on a blocking thread.
    apply(ctx, &changeset, None, move |ws, _| {
        match &content {
            Some(text) => ws.write_atomic(&path, text)?,
            None if !ws.remove(&path)? => {
                return Err(OkfError::Refused {
                    reason: format!("/{path} is already absent in this changeset's view"),
                });
            }
            None => {}
        }
        let message = format!("{path}: reverted to revision {revision}");
        // No replay: a revert is a free edit, which conflicts on a stale
        // base. `path` moves into the outcome (no copy).
        Ok(OpOutcome {
            touched: vec![path],
            replay: BTreeMap::new(),
            message,
            notes: Vec::new(),
        })
    })
    .await
}

/// The changeset a handle names, as diffs against the published text.
///
/// It reads the entries and the published state rather than building the
/// overlay, so a changeset whose overlay no longer applies can still be
/// listed, and then discarded.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the changeset.
///
/// # Returns
/// The changed paths and their diffs.
///
/// # Errors
/// As [`ToolContext::changeset`] with `read`: the key that owns it or an
/// admin may ask; a `read` key owns none.
pub(crate) async fn pending_changes(
    ctx: &ToolContext,
    params: PendingParams,
) -> Result<PendingChanges, OkfError> {
    use crate::store::changesets;

    let Pinned { state, row } = ctx.changeset(&params.changeset, Access::Read).await?;
    let entries = changesets::entries(ctx.app().pool(), row.id).await?;
    let loaded = state.current();
    let files = blocking(move || pending_files(&loaded, &entries)).await;
    Ok(PendingChanges {
        catalog: state.name().to_string(),
        changeset: row.handle,
        files,
    })
}

/// Each entry against the published file (on a blocking thread).
///
/// # Arguments
/// * `loaded` - the published state.
/// * `entries` - the changeset's entries, in path order.
///
/// # Returns
/// One [`PendingFile`] per entry, in the same order.
fn pending_files(
    loaded: &Loaded,
    entries: &[crate::store::changesets::EntryRow],
) -> Vec<PendingFile> {
    // The diffs are independent: rayon's `par_iter` spreads them over the
    // CPU pool, and `collect` keeps the entries' (path) order.
    entries
        .par_iter()
        .map(|entry| {
            let published = loaded.workspace().stored(&entry.path);
            let change = match (&published, &entry.content) {
                (_, None) => "delete",
                (None, Some(_)) => "create",
                (Some(_), Some(_)) => "update",
            };
            let current_revision = loaded.revision(&entry.path);
            let old = published.as_ref().map(|file| file.content.as_slice());
            PendingFile {
                path: format!("/{}", entry.path),
                change,
                base_revision: entry.base_revision,
                current_revision,
                stale: entry.base_revision != current_revision,
                replayable: entry.replay.is_some(),
                diff: unified_diff(&entry.path, old, entry.content.as_deref()),
            }
        })
        .collect()
}

/// A unified diff (3 lines of context) between the published and the
/// pending file. A missing side is `/dev/null`, as `git diff` writes it.
///
/// # Arguments
/// * `path` - catalog-relative path.
/// * `published` - the published bytes, if the file is published.
/// * `pending` - the pending bytes, or `None` for a deletion.
///
/// # Returns
/// The diff text; a one-line "Binary files ... differ" when either side is
/// not UTF-8; a one-line note for a deletion of a path that is not
/// published now (deleted by someone else since the entry was made), or for
/// pending bytes equal to the published ones.
pub(crate) fn unified_diff(path: &str, published: Option<&[u8]>, pending: Option<&[u8]>) -> String {
    if published.is_none() && pending.is_none() {
        return format!(
            "No published {path} to delete: it was deleted after this entry was made\n"
        );
    }
    // Byte for byte the published file (a revert to the revision published
    // now, say): a diff would be empty, so say why.
    if published.is_some() && published == pending {
        return "No change from the published text\n".to_string();
    }
    // A missing side diffs as empty text. A nested `fn`, not a closure:
    // its signature ties the `&str` it returns to the bytes it borrows,
    // which closure inference cannot express.
    fn as_text(bytes: Option<&[u8]>) -> Result<&str, std::str::Utf8Error> {
        bytes.map_or(Ok(""), std::str::from_utf8)
    }
    let (Ok(old), Ok(new)) = (as_text(published), as_text(pending)) else {
        return format!("Binary files a/{path} and b/{path} differ\n");
    };
    let old_header = published.map_or_else(|| "/dev/null".to_string(), |_| format!("a/{path}"));
    let new_header = pending.map_or_else(|| "/dev/null".to_string(), |_| format!("b/{path}"));
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&old_header, &new_header)
        .to_string()
}

#[tool_router(router = changes_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `discard` tool.
    #[tool(
        title = "Discard Changeset Entries",
        description = "Drop entries from the changeset `changeset` names: the named `paths`, or \
                       every entry, which closes the changeset for good (its handle is refused \
                       from then on; call open_changeset for new work). Published documents are \
                       not touched. Write access.",
        annotations(
            title = "Discard Changeset Entries",
            read_only_hint = false,
            destructive_hint = true,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn discard(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<DiscardParams>,
    ) -> Result<Json<Discarded>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(discard(&ctx, params).await)
    }

    /// The `revert` tool.
    #[tool(
        title = "Revert Document",
        description = "Add a document's content at an earlier revision (from `history`), or its \
                       deletion for a `delete` revision, to the changeset `changeset` names, for \
                       review and publish like any other edit. Every write rule applies, so \
                       machinery cannot be reverted. Reverting a file this changeset created to \
                       a `delete` revision drops it from the changeset. Write access.",
        annotations(
            title = "Revert Document",
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = false,
            open_world_hint = false
        )
    )]
    async fn revert(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<RevertParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(revert(&ctx, params).await)
    }

    /// The `pending_changes` tool.
    #[tool(
        title = "Show Pending Changes",
        description = "The unpublished changeset `changeset` names: each changed path \
                       (create, update or delete), the revision it was based on and the one \
                       published now, whether that base is stale, whether publish can replay \
                       it, and a unified diff against the published text.",
        annotations(
            title = "Show Pending Changes",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn pending_changes(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<PendingParams>,
    ) -> Result<Json<PendingChanges>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(pending_changes(&ctx, params).await)
    }
}

#[cfg(test)]
mod open_tests {
    use super::*;

    use crate::config::Config;
    use crate::mcp::testing::{app_with, principal, writer};

    #[tokio::test(flavor = "multi_thread")]
    async fn open_changeset_returns_a_handle_that_resolves() {
        let test = app_with(&[("pg", &[]), ("work", &[])], Config::default()).await;
        let ctx = writer(&test, "alice", "pg").await;
        let reader = crate::mcp::testing::context(&test, principal(&[], Some("pg")), None);

        let opened = open_changeset(&ctx, OpenParams { catalog: None })
            .await
            .unwrap();
        let in_work = open_changeset(
            &ctx,
            OpenParams {
                catalog: Some("work".to_string()),
            },
        )
        .await
        .unwrap();
        let pinned = ctx
            .changeset(&opened.changeset, Access::Write)
            .await
            .unwrap();
        let refused = open_changeset(&reader, OpenParams { catalog: None })
            .await
            .unwrap_err();

        assert_eq!(opened.catalog, "pg");
        assert_eq!(in_work.catalog, "work");
        assert_ne!(opened.changeset, in_work.changeset);
        assert_eq!(pinned.state.name(), "pg");
        assert_eq!(pinned.row.handle, opened.changeset);
        assert_eq!(pinned.row.key_id.as_deref(), Some("alice"));
        assert_eq!(refused.kind(), "Forbidden");
    }

    #[test]
    fn open_changeset_is_listed_with_an_object_output_schema() {
        let tools = OkfServer::tools().list_all();

        let listed = tools
            .iter()
            .find(|tool| tool.name == "open_changeset")
            .expect("open_changeset is registered");

        let schema = listed.output_schema.as_ref().expect("an output schema");
        assert_eq!(schema["type"], "object");
        assert!(listed.input_schema["properties"]
            .as_object()
            .unwrap()
            .contains_key("catalog"));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use chrono::{TimeDelta, Utc};

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, open_at, pending_entry, principal_as, store_key, PendingEdit, TestApp,
    };

    /// `pg` and `work`, and the keys `alice`, `bob` and `root`.
    async fn test_app() -> TestApp {
        let test = app_with(
            &[("pg", &[("a.md", "# A\n")]), ("work", &[])],
            Config::default(),
        )
        .await;
        for key in ["alice", "bob", "root"] {
            store_key(&test, key).await;
        }
        test
    }

    /// A connection of `key` with `access`, reaching `reach` (empty: all).
    fn connection(test: &TestApp, key: &str, access: Access, reach: &[&str]) -> ToolContext {
        context(test, principal_as(key, access, reach, Some("pg")), None)
    }

    /// A new changeset of `ctx`'s key in `pg` with an edit of `a.md`, both
    /// `hours_ago` hours ago; its handle.
    async fn edit_hours_ago(test: &TestApp, ctx: &ToolContext, hours_ago: i64) -> String {
        let at = Utc::now() - TimeDelta::hours(hours_ago);
        let opened = open_at(test, ctx, "pg", at).await;
        let edit = PendingEdit {
            path: "a.md",
            text: Some("# Pending\n"),
            at,
        };
        pending_entry(test, &opened.handle, &edit).await.handle
    }

    /// The handles listed, in order.
    fn handles(listed: &Listed) -> Vec<&str> {
        listed
            .changesets
            .iter()
            .map(|changeset| changeset.changeset.as_str())
            .collect()
    }

    fn all() -> ListParams {
        ListParams { catalog: None }
    }

    #[test]
    fn the_idle_cutoff_is_now_less_the_timeout_and_never_overflows() {
        let now = Utc::now();

        let cutoff = idle_cutoff(now, std::time::Duration::from_secs(7200));
        let huge = idle_cutoff(now, std::time::Duration::MAX);

        assert_eq!(cutoff, now - TimeDelta::hours(2));
        assert_eq!(huge, chrono::DateTime::<Utc>::MIN_UTC);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_changesets_lists_only_the_connection_s_catalogs() {
        let test = test_app().await;
        let alice = connection(&test, "alice", Access::Write, &[]);
        let in_pg = open_at(&test, &alice, "pg", Utc::now()).await.handle;
        let in_work = open_at(&test, &alice, "work", Utc::now()).await.handle;
        let work_only = context(
            &test,
            principal_as("alice", Access::Write, &[], Some("pg")),
            Some(&["work"]),
        );

        let unlisted = list_changesets(&alice, all()).await.unwrap();
        let listed = list_changesets(&work_only, all()).await.unwrap();

        assert_eq!(unlisted.changesets.len(), 2);
        assert!(handles(&unlisted).contains(&in_pg.as_str()));
        assert_eq!(handles(&listed), [in_work.as_str()]);
        // `nope` does not exist; the second key reaches only `work`, not `pg`.
        for (list, reach) in [(["work", "nope"], &[][..]), (["work", "pg"], &["work"][..])] {
            let on_list = context(
                &test,
                principal_as("alice", Access::Write, reach, Some("work")),
                Some(&list),
            );

            let refused = list_changesets(&on_list, all()).await.unwrap_err();

            assert_eq!(refused.kind(), "CatalogNotSelected", "{list:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_changesets_shows_a_key_its_own_and_an_admin_everyone_s() {
        let test = test_app().await;
        let alice = connection(&test, "alice", Access::Write, &[]);
        let empty = open_at(&test, &alice, "pg", Utc::now()).await.handle;
        // Three hours is past the default `session_idle_timeout` of two.
        let bobs = edit_hours_ago(&test, &connection(&test, "bob", Access::Write, &[]), 3).await;
        let admin = connection(&test, "root", Access::Admin, &[]);

        let by_alice = list_changesets(&alice, all()).await.unwrap();
        let by_admin = list_changesets(&admin, all()).await.unwrap();
        let by_admin_of_work =
            list_changesets(&connection(&test, "root", Access::Admin, &["work"]), all())
                .await
                .unwrap();
        let narrowed = list_changesets(
            &admin,
            ListParams {
                catalog: Some("work".to_string()),
            },
        )
        .await
        .unwrap();
        let unreachable = list_changesets(
            &connection(&test, "alice", Access::Write, &["work"]),
            ListParams {
                catalog: Some("pg".to_string()),
            },
        )
        .await;

        assert_eq!(handles(&by_alice), [empty.as_str()]);
        let own = &by_alice.changesets[0];
        assert_eq!(
            (
                own.catalog.as_str(),
                own.key_name.as_deref(),
                own.key.as_deref(),
                own.actor.as_str()
            ),
            ("pg", Some("alice"), Some("alice"), "alice")
        );
        assert!(own.paths.is_empty(), "an empty changeset is listed too");
        assert!(!own.idle);
        assert_eq!(
            handles(&by_admin),
            [bobs.as_str(), empty.as_str()],
            "oldest change first"
        );
        let old = &by_admin.changesets[0];
        assert_eq!(old.key.as_deref(), Some("bob"));
        assert_eq!(old.paths, ["/a.md"]);
        assert!(old.idle);
        assert!(old.last_change < own.last_change);
        assert!(by_admin_of_work.changesets.is_empty());
        assert!(narrowed.changesets.is_empty());
        assert_eq!(unreachable.unwrap_err().kind(), "CatalogNotSelected");
    }

    #[test]
    fn list_changesets_is_listed_and_the_old_tools_are_gone() {
        let tools = OkfServer::tools().list_all();

        let listed = tools
            .iter()
            .find(|tool| tool.name == "list_changesets")
            .expect("list_changesets is registered");
        let schema = listed.output_schema.as_ref().expect("an output schema");
        assert_eq!(schema["type"], "object");
        for gone in ["stranded_sessions", "adopt_changeset"] {
            assert!(tools.iter().all(|tool| tool.name != gone), "{gone}");
        }
    }
}

#[cfg(test)]
mod publish_tests {
    use super::*;
    use std::time::Duration;

    use chrono::Utc;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{
        app_with, changeset_row, context, files, open, principal_as, store_key, writer, TestApp,
    };
    use crate::mcp::writes::{replace_body, ReplaceBodyParams};
    use crate::okf::ops::{self, LogKind};
    use crate::store::changesets::{self, ChangesetState, EntryRow};
    use crate::store::import::{import, ImportOptions, ImportScope};
    use crate::store::keys::Access;
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

    async fn body(ctx: &ToolContext, handle: &str, path: &str, text: &str) -> WriteReport {
        let params = ReplaceBodyParams {
            path: path.to_string(),
            body: text.to_string(),
            changeset: handle.to_string(),
        };
        replace_body(ctx, params).await.unwrap()
    }

    /// A `log` write, through the write path as Task 9's tool makes it.
    async fn log_entry(ctx: &ToolContext, handle: &str, text: &str) -> WriteReport {
        let text = text.to_string();
        apply(ctx, handle, None, move |workspace, op| {
            ops::log(workspace, op, LogKind::Update, &text)
        })
        .await
        .unwrap()
    }

    /// An `index` write, through the write path as Task 9's tool makes it.
    async fn list(ctx: &ToolContext, handle: &str, path: &str, summary: &str) -> WriteReport {
        let (path, summary) = (path.to_string(), summary.to_string());
        apply(ctx, handle, None, move |workspace, op| {
            ops::index(workspace, op, &path, &summary, None, None)
        })
        .await
        .unwrap()
    }

    async fn publish_as(
        ctx: &ToolContext,
        handle: &str,
        message: &str,
    ) -> Result<Published, OkfError> {
        let params = PublishParams {
            message: message.to_string(),
            changeset: handle.to_string(),
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

    async fn state_of(test: &TestApp, handle: &str) -> ChangesetState {
        changeset_row(test, handle).await.state
    }

    /// Stores an entry directly, as no write tool would (a rule changed, a
    /// hand edit), to show publish checks again.
    async fn insert_entry(test: &TestApp, handle: &str, entry: EntryRow) {
        let state = test.app.catalog("pg").unwrap();
        let row = changeset_row(test, handle).await;
        let mut tx = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut tx, state.id()).await.unwrap();
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

    /// A publish whose changeset closed after the call resolved it (another
    /// publish won the race) is refused under the lock, naming the handle,
    /// never the internal id clients don't see.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_of_a_changeset_closed_meanwhile_names_its_handle() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        body(&ctx, &handle, "/a.md", "# A\n\nNew.\n").await;
        // Resolved while open, as the losing publish read it before the lock.
        let stale = changeset_row(&test, &handle).await;
        let state = test.app.catalog("pg").unwrap();
        publish_as(&ctx, &handle, "first").await.unwrap();

        let refused = crate::store::publish::publish(&test.app, &state, &stale, "a", "second")
            .await
            .unwrap_err();

        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(message.contains(&handle), "{message}");
        assert!(
            !message.contains(&format!("changeset {} ", stale.id)),
            "{message}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn publish_writes_revisions_bumps_the_generation_and_swaps_the_snapshot() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let state = test.app.catalog("pg").unwrap();
        let before = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;
        let written = body(&ctx, &handle, "/a.md", "# A\n\nNew.\n").await;

        let published = publish_as(&ctx, &handle, "edit a").await.unwrap();

        assert!(published.report.published);
        assert_eq!(published.changeset, written.changeset);
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
        assert_eq!(changeset_id, Some(changeset_row(&test, &handle).await.id));
        let updated_by: String =
            sqlx::query_scalar("SELECT updated_by FROM documents WHERE path = 'a.md'")
                .fetch_one(test.app.pool())
                .await
                .unwrap();
        assert_eq!(updated_by, "writer-a");
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Published);
        // The handle is spent: reading or writing through it is refused.
        let spent = ctx.pin(Some(&handle)).await.unwrap_err();
        assert_eq!(spent.kind(), "Refused");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_may_publish_another_key_s_changeset() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        body(&ctx, &handle, "/a.md", "# A\n\nBy a.\n").await;
        store_key(&test, "root").await;
        let admin = context(
            &test,
            principal_as("root", Access::Admin, &[], Some("pg")),
            None,
        );

        let published = publish_as(&admin, &handle, "take over").await.unwrap();

        assert!(published.report.published);
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Published);
        let state = test.app.catalog("pg").unwrap();
        let rows = history::revisions(test.app.pool(), state.id(), "a.md", 5)
            .await
            .unwrap();
        // The publishing key's actor, not the opener's.
        assert_eq!(
            (rows[0].actor.as_str(), rows[0].message.as_str()),
            ("root", "take over")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_changeset_is_no_longer_listed() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let published = open(&ctx).await;
        let kept = open(&ctx).await;
        body(&ctx, &published, "/a.md", "# A\n\nNew.\n").await;
        let all = || ListParams { catalog: None };
        let before = list_changesets(&ctx, all()).await.unwrap();

        publish_as(&ctx, &published, "edit a").await.unwrap();
        let after = list_changesets(&ctx, all()).await.unwrap();

        let listed = |listed: &Listed| -> Vec<String> {
            listed
                .changesets
                .iter()
                .map(|changeset| changeset.changeset.clone())
                .collect()
        };
        let mut both = listed(&before);
        both.sort_unstable();
        let mut expected = vec![published.clone(), kept.clone()];
        expected.sort_unstable();
        assert_eq!(both, expected);
        assert_eq!(listed(&after), [kept]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn nothing_to_publish_is_a_result_and_a_message_is_required() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let before = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;

        let nothing = publish_as(&ctx, &handle, "nothing").await.unwrap();
        let blank = publish_as(&ctx, &handle, "  ").await.unwrap_err();
        // The message check comes first, so even a made-up handle gets it.
        let blank_unknown = publish_as(&ctx, "made-up", " ").await.unwrap_err();
        let unknown = publish_as(&ctx, "made-up", "x").await.unwrap_err();

        assert!(!nothing.report.published);
        assert_eq!(nothing.changeset, handle);
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Open);
        assert_eq!(blank_unknown.kind(), "Refused");
        assert_eq!(unknown.kind(), "UnknownChangeset");
        assert_eq!(blank.kind(), "Refused");
        let after = catalogs::find(test.app.pool(), "pg")
            .await
            .unwrap()
            .generation;
        assert_eq!(after, before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_on_a_stale_snapshot_needs_one_connection() {
        // Arrange: an import moves the database past the server's snapshot
        // (no refresh), and every pooled connection but one is taken.
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        body(&ctx, &handle, "/a.md", "Edited.\n").await;
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "t",
            now: Utc::now(),
        };
        let republished = files(&[
            ("a.md", NOTE),
            ("hub.md", HUB),
            ("infra/index.md", "# Infra\n"),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("log.md", "# Log\n"),
            ("z.md", "# Z\n"),
        ]);
        import(test.app.pool(), "pg", republished, &options)
            .await
            .unwrap();
        let state = test.app.catalog("pg").unwrap();
        let stale = state.current().generation();
        let size = test.app.pool().options().get_max_connections();
        let mut held = Vec::with_capacity(size as usize);
        for _ in 1..size {
            held.push(test.app.pool().acquire().await.unwrap());
        }

        // Act: well inside sqlx's 30 s acquire timeout, which a publish
        // waiting for a second connection would run into.
        let published = tokio::time::timeout(
            Duration::from_secs(10),
            publish_as(&ctx, &handle, "on a stale snapshot"),
        )
        .await
        .expect("the publish needs no second connection");
        drop(held);

        // Assert
        assert!(published.unwrap().report.published);
        assert!(state.current().generation() > stale);
        assert!(state.current().snapshot().get("z.md").is_some());
        assert!(published_text(&test, "a.md").ends_with("Edited.\n"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_stale_free_edit_conflicts_and_leaves_the_changeset_open() {
        let test = pg().await;
        let first = writer(&test, "writer-a", "pg").await;
        let second = writer(&test, "writer-b", "pg").await;
        let (first_handle, mine) = (open(&first).await, open(&second).await);
        body(&first, &first_handle, "/a.md", "From a.\n").await;
        body(&second, &mine, "/a.md", "From b.\n").await;
        publish_as(&first, &first_handle, "a first").await.unwrap();

        let error = publish_as(&second, &mine, "b second").await.unwrap_err();

        assert!(matches!(
            error,
            OkfError::Conflict { ref files } if files == &[ConflictFile {
                path: "/a.md".to_string(),
                base_revision: Some(1),
                current_revision: Some(2),
            }]
        ));
        assert_eq!(state_of(&test, &mine).await, ChangesetState::Open);
        let entries = changesets::entries(test.app.pool(), changeset_row(&test, &mine).await.id)
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
        let (a, b) = (open(&first).await, open(&second).await);
        log_entry(&first, &a, "from a").await;
        log_entry(&second, &b, "from b").await;

        // Both at once: the publish lock runs them one after the other, and
        // the second replays its entry onto the first's log.md.
        let (one, two) = tokio::join!(publish_as(&first, &a, "a"), publish_as(&second, &b, "b"));

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
        let (a, b) = (open(&first).await, open(&second).await);
        list(&first, &a, "/infra/x.md", "the x").await;
        list(&second, &b, "/infra/y.md", "the y").await;
        publish_as(&first, &a, "list x").await.unwrap();

        let published = publish_as(&second, &b, "list y").await.unwrap();

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
        let handle = open(&ctx).await;
        body(&ctx, &handle, "/a.md", "See [gone](/gone.md).\n").await;

        let error = publish_as(&ctx, &handle, "broken").await.unwrap_err();

        // `&error`: the pattern borrows, so the `else` branch can still print it.
        let OkfError::LintFailed { errors, .. } = &error else {
            panic!("not LintFailed: {error:?}")
        };
        assert!(errors
            .iter()
            .any(|finding| finding.file == "a.md" && finding.rule == "broken-link"));
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Open);
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deletion_still_linked_from_elsewhere_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let deletion = EntryRow {
            path: "infra/x.md".to_string(),
            base_revision: Some(1),
            content: None,
            executable: false,
            replay: None,
        };
        insert_entry(&test, &handle, deletion).await;

        let error = publish_as(&ctx, &handle, "drop x").await.unwrap_err();

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
        let handle = open(&ctx).await;
        let deletion = EntryRow {
            path: "infra/y.md".to_string(),
            base_revision: Some(1),
            content: None,
            executable: false,
            replay: None,
        };
        insert_entry(&test, &handle, deletion).await;
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
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "cli",
            now: Utc::now(),
        };
        import(test.app.pool(), "pg", files(&incoming), &options)
            .await
            .unwrap();
        let state = test.app.catalog("pg").unwrap();
        let stale = state.current().generation();

        let error = publish_as(&ctx, &handle, "drop y").await.unwrap_err();

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
        let handle = open(&ctx).await;
        // Built at run time, so no source file holds a secret-shaped string.
        let leaked = format!("{NOTE}password = {}\n", "Ab1".repeat(4));
        let entry = EntryRow {
            path: "a.md".to_string(),
            base_revision: Some(1),
            content: Some(leaked.into_bytes()),
            executable: false,
            replay: None,
        };
        insert_entry(&test, &handle, entry).await;

        let error = publish_as(&ctx, &handle, "leak").await.unwrap_err();

        assert_eq!(error.kind(), "SecretDetected");
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    /// `(revision, deleted, content)` of a document row.
    async fn document_row(test: &TestApp, path: &str) -> (i32, bool, Vec<u8>) {
        sqlx::query_as(
            "SELECT revision, deleted_at IS NOT NULL, content FROM documents WHERE path = $1",
        )
        .bind(path)
        .fetch_one(test.app.pool())
        .await
        .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_publish_creates_deletes_and_continues_a_tombstone_s_number() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let created = "---\ntype: Note\ntitle: B\ndescription: B.\n---\n# B\n";
        let new_file = EntryRow {
            path: "b.md".to_string(),
            base_revision: None,
            content: Some(created.as_bytes().to_vec()),
            executable: false,
            replay: None,
        };
        // Nothing links to infra/y.md, so its deletion passes the link check.
        let deletion = EntryRow {
            path: "infra/y.md".to_string(),
            base_revision: Some(1),
            content: None,
            executable: false,
            replay: None,
        };
        insert_entry(&test, &handle, new_file).await;
        insert_entry(&test, &handle, deletion).await;

        let first = publish_as(&ctx, &handle, "add b, drop y").await.unwrap();
        // A tombstone: the number moved on, the content and the snapshot entry are gone.
        let tombstone = document_row(&test, "infra/y.md").await;
        let gone = test.app.catalog("pg").unwrap().current();
        assert_eq!(tombstone, (2, true, Vec::new()));
        assert!(gone.snapshot().get("infra/y.md").is_none());
        // The changeset is closed, so a new one brings y back on its tombstone.
        let again_handle = open(&ctx).await;
        let again = EntryRow {
            path: "infra/y.md".to_string(),
            base_revision: None,
            content: Some(Y.as_bytes().to_vec()),
            executable: false,
            replay: None,
        };
        insert_entry(&test, &again_handle, again).await;
        let second = publish_as(&ctx, &again_handle, "bring y back")
            .await
            .unwrap();

        let revision = |path: &str, revision: i32, op: &'static str| PublishedRevision {
            path: path.to_string(),
            revision,
            op,
        };
        assert_eq!(
            first.report.revisions,
            [
                revision("/b.md", 1, "create"),
                revision("/infra/y.md", 2, "delete")
            ]
        );
        assert_eq!(
            second.report.revisions,
            [revision("/infra/y.md", 3, "create")]
        );
        assert_eq!(published_text(&test, "b.md"), created);
        assert_eq!(published_text(&test, "infra/y.md"), Y);
        assert_eq!(
            document_row(&test, "infra/y.md").await,
            (3, false, Y.as_bytes().to_vec())
        );
        // The deletion's history row: no content, and the changeset that made it.
        let deleted: (Option<Vec<u8>>, Option<i64>) = sqlx::query_as(
            "SELECT content, changeset_id FROM revisions WHERE path = 'infra/y.md' AND revision = 2",
        )
        .fetch_one(test.app.pool())
        .await
        .unwrap();
        assert_eq!(
            deleted,
            (None, Some(changeset_row(&test, &first.changeset).await.id))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revision_written_meanwhile_is_a_conflict_not_a_database_error() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let state = test.app.catalog("pg").unwrap();
        body(&ctx, &handle, "/a.md", "# A\n\nMine.\n").await;
        // A revision 2 of a.md that the publish lock should have ruled out
        // (a hand edit of the database): the backstop must report it.
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                    message, created_at) \
             VALUES ($1, 'a.md', 2, 'update', 'x'::bytea, false, 'intruder', 'x', now())",
        )
        .bind(state.id())
        .execute(test.app.pool())
        .await
        .unwrap();

        let error = publish_as(&ctx, &handle, "mine").await.unwrap_err();

        assert!(matches!(
            error,
            // The live revision, re-read after the rollback: the stray row
            // never moved the document past 1.
            OkfError::Conflict { ref files } if files == &[ConflictFile {
                path: "/a.md".to_string(),
                base_revision: Some(1),
                current_revision: Some(1),
            }]
        ));
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Open);
        assert_eq!(document_row(&test, "a.md").await.0, 1);
        assert_eq!(published_text(&test, "a.md"), NOTE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_queued_behind_a_publish_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let state = test.app.catalog("pg").unwrap();
        body(&ctx, &handle, "/a.md", "# A\n\nFirst.\n").await;
        // Hold the changeset's row lock, so the publish and then the write
        // queue on it in that order.
        let mut holder = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut holder, state.id())
            .await
            .unwrap();
        changesets::lock(&mut holder, changeset_row(&test, &handle).await.id)
            .await
            .unwrap();
        // Two connections of one key, each with its own context and copy of
        // the handle (`ToolContext` is not `Clone`); `move` gives each task its own.
        let same_key = || {
            (
                context(
                    &test,
                    principal_as("writer-a", Access::Write, &[], Some("pg")),
                    None,
                ),
                handle.clone(),
            )
        };
        let (publisher, to_publish) = same_key();
        let publishing =
            tokio::spawn(async move { publish_as(&publisher, &to_publish, "first").await });
        lock_waiters(&test, 1).await;
        let (late, to_write) = same_key();
        let writing = tokio::spawn(async move {
            let params = ReplaceBodyParams {
                path: "/infra/y.md".to_string(),
                body: "# Y\n\nLate.\n".to_string(),
                changeset: to_write,
            };
            replace_body(&late, params).await
        });
        lock_waiters(&test, 2).await;

        holder.rollback().await.unwrap();
        let published = publishing.await.unwrap().unwrap();
        let refused = writing.await.unwrap().unwrap_err();

        // The publish took the lock first and closed the changeset. The
        // write re-reads the row once its lock is granted, sees it
        // published, and is refused: never `Busy`, and never stored.
        assert_eq!(published.changeset, handle);
        assert_eq!(
            published.report.revisions,
            [PublishedRevision {
                path: "/a.md".to_string(),
                revision: 2,
                op: "update"
            }]
        );
        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(message.contains("while this write waited"), "{message}");
        assert_eq!(published_text(&test, "infra/y.md"), Y);
        let entries: i64 =
            sqlx::query_scalar("SELECT count(*) FROM changeset_entries WHERE path = 'infra/y.md'")
                .fetch_one(test.app.pool())
                .await
                .unwrap();
        assert_eq!(entries, 0);
    }
}

#[cfg(test)]
mod changes_tests {
    use super::*;
    use std::time::Duration;

    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, changeset_row, context, files, open, pending_entry, principal, principal_as,
        writer, PendingEdit, TestApp,
    };
    use crate::mcp::writes::{
        create_concept, replace_body, CreateConceptParams, ReplaceBodyParams,
    };
    use crate::store::changesets::{self, ChangesetState};
    use crate::store::import::{import, ImportOptions, ImportScope};

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
    const NOTE_B: &str = "---\ntype: Note\ntitle: B\ndescription: B note.\n---\n# B\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[
            ("a.md", NOTE),
            ("b.md", NOTE_B),
            ("AGENTS.md", "# Agents\n"),
        ];
        app_with(&[("pg", files)], Config::default()).await
    }

    /// Replaces the published catalog with `entries` (an import standing in
    /// for other sessions' publishes), and lets the server notice.
    async fn republish(test: &TestApp, entries: &[(&str, &str)]) {
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "tester",
            now: Utc::now(),
        };
        import(test.app.pool(), "pg", files(entries), &options)
            .await
            .unwrap();
        test.app
            .catalog("pg")
            .unwrap()
            .refresh(test.app.pool())
            .await
            .unwrap();
    }

    fn body_params(handle: &str, path: &str, text: &str) -> ReplaceBodyParams {
        ReplaceBodyParams {
            path: path.to_string(),
            body: text.to_string(),
            changeset: handle.to_string(),
        }
    }

    async fn body(ctx: &ToolContext, handle: &str, path: &str, text: &str) -> WriteReport {
        replace_body(ctx, body_params(handle, path, text))
            .await
            .unwrap()
    }

    async fn new_note(ctx: &ToolContext, handle: &str, path: &str) -> WriteReport {
        let frontmatter =
            serde_json::json!({"type": "Note", "title": "New", "description": "New."});
        let params = CreateConceptParams {
            path: path.to_string(),
            frontmatter: frontmatter.as_object().unwrap().clone(),
            body: "# New\n".to_string(),
            index_summary: None,
            index_section: None,
            by: None,
            changeset: handle.to_string(),
        };
        create_concept(ctx, params).await.unwrap()
    }

    fn revert_to(handle: &str, path: &str, revision: i32) -> RevertParams {
        RevertParams {
            path: path.to_string(),
            revision,
            changeset: handle.to_string(),
        }
    }

    fn discard_paths(handle: &str, paths: Option<&[&str]>) -> DiscardParams {
        let paths = paths.map(|paths| paths.iter().map(|path| path.to_string()).collect());
        DiscardParams {
            paths,
            changeset: handle.to_string(),
        }
    }

    fn pending_params(handle: &str) -> PendingParams {
        PendingParams {
            changeset: handle.to_string(),
        }
    }

    async fn pending(ctx: &ToolContext, handle: &str) -> PendingChanges {
        pending_changes(ctx, pending_params(handle)).await.unwrap()
    }

    async fn state_of(test: &TestApp, handle: &str) -> ChangesetState {
        changeset_row(test, handle).await.state
    }

    /// The paths of the changeset `handle` names, in order.
    async fn entry_paths(test: &TestApp, handle: &str) -> Vec<String> {
        let row = changeset_row(test, handle).await;
        changesets::entries(test.app.pool(), row.id)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect()
    }

    /// The writer key `writer-a` demoted to `read`: it still owns its changesets.
    fn demoted(test: &TestApp) -> ToolContext {
        context(
            test,
            principal_as("writer-a", Access::Read, &[], Some("pg")),
            None,
        )
    }

    /// Waits until `count` backends wait on a lock (as `publish_tests` does).
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
    async fn discard_drops_named_paths_or_closes_the_whole_changeset() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        body(&ctx, &handle, "/a.md", "One.\n").await;
        body(&ctx, &handle, "/b.md", "Two.\n").await;
        let before = changeset_row(&test, &handle).await.version;

        let empty = discard(&ctx, discard_paths(&handle, Some(&[])))
            .await
            .unwrap();
        let unchanged = changeset_row(&test, &handle).await.version;
        let some = discard(&ctx, discard_paths(&handle, Some(&["/a.md", "/c.md"])))
            .await
            .unwrap();
        let left = pending(&ctx, &handle).await;
        let outside = discard(&ctx, discard_paths(&handle, Some(&["/../x.md"])))
            .await
            .unwrap_err();
        let all = discard(&ctx, discard_paths(&handle, None)).await.unwrap();
        // A full discard kills the handle: every later call is refused.
        let after = pending_changes(&ctx, pending_params(&handle))
            .await
            .unwrap_err();
        let twice = discard(&ctx, discard_paths(&handle, None))
            .await
            .unwrap_err();
        let late = replace_body(&ctx, body_params(&handle, "/a.md", "Three.\n"))
            .await
            .unwrap_err();

        // An empty list drops nothing and leaves the version alone.
        assert_eq!(
            (empty.changeset.as_str(), empty.removed, empty.closed),
            (handle.as_str(), 0, false)
        );
        assert_eq!(unchanged, before);
        assert_eq!(
            (some.changeset.as_str(), some.removed, some.closed),
            (handle.as_str(), 1, false)
        );
        let paths: Vec<&str> = left.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["/b.md"]);
        assert_eq!(outside.kind(), "InvalidPath");
        assert_eq!((all.removed, all.closed), (1, true));
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Discarded);
        for refused in [after, twice, late] {
            assert_eq!(refused.kind(), "Refused");
            let message = refused.to_string();
            assert!(
                message.contains(&handle) && message.contains("discarded"),
                "{message}"
            );
        }
        // The published documents are untouched.
        let loaded = test.app.catalog("pg").unwrap().current();
        assert_eq!(
            loaded.snapshot().get("a.md").map(|doc| doc.text()),
            Some(NOTE)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn discard_needs_the_owner_s_write_access() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let reader = context(&test, principal(&[], Some("pg")), None);

        let unknown = discard(&reader, discard_paths(&handle, None))
            .await
            .unwrap_err();
        let forbidden = discard(&demoted(&test), discard_paths(&handle, None))
            .await
            .unwrap_err();

        // A `read` key owns no changeset; the owner below `write` is refused.
        assert_eq!(unknown.kind(), "UnknownChangeset");
        assert_eq!(forbidden.kind(), "Forbidden");
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_queued_behind_a_discard_is_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let state = test.app.catalog("pg").unwrap();
        body(&ctx, &handle, "/a.md", "First.\n").await;
        // Hold the changeset's row lock, so the discard and then the write
        // queue on it in that order.
        let mut holder = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut holder, state.id())
            .await
            .unwrap();
        changesets::lock(&mut holder, changeset_row(&test, &handle).await.id)
            .await
            .unwrap();
        // Two connections of one key, each with its own context and copy of
        // the handle (`ToolContext` is not `Clone`); `move` gives each task its own.
        let same_key = || {
            (
                context(
                    &test,
                    principal_as("writer-a", Access::Write, &[], Some("pg")),
                    None,
                ),
                handle.clone(),
            )
        };
        let (discarder, to_discard) = same_key();
        let discarding =
            tokio::spawn(
                async move { discard(&discarder, discard_paths(&to_discard, None)).await },
            );
        lock_waiters(&test, 1).await;
        let (late, to_write) = same_key();
        let writing = tokio::spawn(async move {
            replace_body(&late, body_params(&to_write, "/b.md", "Late.\n")).await
        });
        lock_waiters(&test, 2).await;

        holder.rollback().await.unwrap();
        let discarded = discarding.await.unwrap().unwrap();
        let refused = writing.await.unwrap().unwrap_err();

        // The discard took the lock first and closed the changeset. The
        // write re-reads the row once its lock is granted, sees it
        // discarded, and is refused: never `Busy`, and nothing stored.
        assert_eq!(
            (
                discarded.changeset.as_str(),
                discarded.removed,
                discarded.closed
            ),
            (handle.as_str(), 1, true)
        );
        assert_eq!(state_of(&test, &handle).await, ChangesetState::Discarded);
        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(message.contains("while this write waited"), "{message}");
        assert!(entry_paths(&test, &handle).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revert_writes_an_old_revision_as_a_free_edit() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        republish(
            &test,
            &[
                ("a.md", "# Two\n"),
                ("b.md", NOTE_B),
                ("AGENTS.md", "# Agents\n"),
            ],
        )
        .await;

        let reverted = revert(&ctx, revert_to(&handle, "/a.md", 1)).await.unwrap();
        let shown = pending(&ctx, &handle).await;
        republish(
            &test,
            &[
                ("a.md", "# Three\n"),
                ("b.md", NOTE_B),
                ("AGENTS.md", "# Agents\n"),
            ],
        )
        .await;
        let stale = pending(&ctx, &handle).await;

        assert_eq!(reverted.touched, ["/a.md"]);
        assert_eq!(reverted.message, "a.md: reverted to revision 1");
        let entries = changesets::entries(test.app.pool(), changeset_row(&test, &handle).await.id)
            .await
            .unwrap();
        assert_eq!(entries[0].content.as_deref(), Some(NOTE.as_bytes()));
        // Based on what is published now (revision 2), with no replay.
        assert_eq!(
            (entries[0].base_revision, entries[0].replay.as_ref()),
            (Some(2), None)
        );
        let file = &shown.files[0];
        assert_eq!(
            (file.change, file.base_revision, file.current_revision),
            ("update", Some(2), Some(2))
        );
        assert!(!file.stale && !file.replayable);
        assert!(
            file.diff.starts_with("--- a/a.md\n+++ b/a.md\n"),
            "{}",
            file.diff
        );
        assert!(file.diff.contains("-# Two\n+---\n"), "{}", file.diff);
        // Published again since: the base is stale, and the diff is against revision 3.
        let file = &stale.files[0];
        assert_eq!(
            (file.base_revision, file.current_revision, file.stale),
            (Some(2), Some(3), true)
        );
        assert!(file.diff.contains("-# Three\n"), "{}", file.diff);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revert_to_a_delete_revision_deletes_and_machinery_stays_refused() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        // b.md: 1 imported, 2 deleted, 3 imported again.
        republish(&test, &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")]).await;
        republish(
            &test,
            &[
                ("a.md", NOTE),
                ("b.md", NOTE_B),
                ("AGENTS.md", "# Agents\n"),
            ],
        )
        .await;

        let deleted = revert(&ctx, revert_to(&handle, "/b.md", 2)).await.unwrap();
        let twice = revert(&ctx, revert_to(&handle, "/b.md", 2))
            .await
            .unwrap_err();
        let machinery = revert(&ctx, revert_to(&handle, "/AGENTS.md", 1))
            .await
            .unwrap_err();
        let no_such = revert(&ctx, revert_to(&handle, "/a.md", 9))
            .await
            .unwrap_err();
        // A revision whose bytes are not UTF-8 (no write tool stores one).
        sqlx::query("UPDATE revisions SET content = '\\xff'::bytea WHERE path = 'a.md'")
            .execute(test.app.pool())
            .await
            .unwrap();
        let binary = revert(&ctx, revert_to(&handle, "/a.md", 1))
            .await
            .unwrap_err();
        let read_only = revert(&demoted(&test), revert_to(&handle, "/a.md", 1))
            .await
            .unwrap_err();

        assert_eq!(deleted.touched, ["/b.md"]);
        let entries = changesets::entries(test.app.pool(), changeset_row(&test, &handle).await.id)
            .await
            .unwrap();
        assert_eq!(
            (entries[0].path.as_str(), entries[0].content.as_ref()),
            ("b.md", None)
        );
        assert_eq!(entries[0].base_revision, Some(3));
        let shown = pending(&ctx, &handle).await;
        assert_eq!(shown.files[0].change, "delete");
        assert!(
            shown.files[0]
                .diff
                .starts_with("--- a/b.md\n+++ /dev/null\n"),
            "{}",
            shown.files[0].diff
        );
        assert_eq!(twice.kind(), "Refused");
        assert_eq!(machinery.kind(), "InvalidPath");
        assert_eq!(no_such.kind(), "NotFound");
        assert_eq!(binary.kind(), "Refused");
        assert_eq!(read_only.kind(), "Forbidden");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reverting_a_created_file_to_absent_leaves_no_entry() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        // b.md: 1 imported, 2 deleted. The changeset then creates it anew.
        republish(&test, &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")]).await;
        let created = new_note(&ctx, &handle, "/b.md").await;

        let reverted = revert(&ctx, revert_to(&handle, "/b.md", 2)).await.unwrap();

        assert_eq!(created.touched, ["/b.md"]);
        // Never published as the changeset made it and not published now: the
        // changeset's own entry goes, so nothing is left to list.
        assert!(reverted.touched.is_empty(), "{:?}", reverted.touched);
        assert_eq!(reverted.changeset, created.changeset);
        assert!(entry_paths(&test, &handle).await.is_empty());
        assert!(pending(&ctx, &handle).await.files.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_changes_diffs_a_new_file_against_dev_null_and_needs_only_read() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        new_note(&ctx, &handle, "/new.md").await;

        let shown = pending(&ctx, &handle).await;
        // The owner below `write` may still look; a `read` key owns none.
        let owner_reading = pending(&demoted(&test), &handle).await;
        let reader = context(&test, principal(&[], Some("pg")), None);
        let unknown = pending_changes(&reader, pending_params(&handle))
            .await
            .unwrap_err();

        assert_eq!(
            (shown.catalog.as_str(), shown.changeset.as_str()),
            ("pg", handle.as_str())
        );
        let file = &shown.files[0];
        assert_eq!(
            (file.path.as_str(), file.change, file.base_revision),
            ("/new.md", "create", None)
        );
        assert_eq!((file.current_revision, file.stale), (None, false));
        assert!(
            file.diff
                .starts_with("--- /dev/null\n+++ b/new.md\n@@ -0,0 +1,"),
            "{}",
            file.diff
        );
        assert!(file.diff.contains("+# New\n"));
        assert_eq!(owner_reading, shown);
        assert_eq!(unknown.kind(), "UnknownChangeset");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_changeset_whose_overlay_no_longer_applies_can_be_listed_and_discarded() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let handle = open(&ctx).await;
        let state = test.app.catalog("pg").unwrap();
        // `a.md` is a published file, so an entry under it can never apply
        // (no write tool stores one; a publish made after the edit can).
        let edit = PendingEdit {
            path: "a.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        // Pinned per read, as each read call does.
        let pinned = || async { ctx.pin(Some(&handle)).await.unwrap() };

        let broken = ctx.view(&state, pinned().await.as_ref()).await.err();
        let shown = pending(&ctx, &handle).await;
        let dropped = discard(&ctx, discard_paths(&handle, Some(&["/a.md/x.md"])))
            .await
            .unwrap();
        let fixed = ctx.view(&state, pinned().await.as_ref()).await;

        assert!(
            matches!(broken, Some(OkfError::InvalidPath { .. })),
            "{broken:?}"
        );
        let paths: Vec<&str> = shown.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["/a.md/x.md"]);
        assert_eq!(dropped.removed, 1);
        assert!(
            fixed.is_ok(),
            "the overlay applies again once the entry is gone"
        );
    }

    #[test]
    fn a_file_equal_to_the_published_one_is_a_one_line_note() {
        let text = Some(&b"# A\n"[..]);

        let same = unified_diff("a.md", text, text);
        let binary = unified_diff("logo.png", Some(b"\x89PNG\xff"), Some(b"\x89PNG\xff"));

        assert_eq!(same, "No change from the published text\n");
        assert_eq!(binary, "No change from the published text\n");
    }

    #[test]
    fn deleting_a_path_deleted_since_is_a_one_line_note() {
        let diff = unified_diff("a.md", None, None);

        assert_eq!(
            diff,
            "No published a.md to delete: it was deleted after this entry was made\n"
        );
    }

    #[test]
    fn a_file_that_is_not_text_is_reported_as_binary() {
        let diff = unified_diff("logo.png", Some(b"\x89PNG\xff"), Some(b"\x89PNG\xfe"));

        assert_eq!(diff, "Binary files a/logo.png and b/logo.png differ\n");
    }

    #[test]
    fn a_stored_path_is_normalized_and_stays_inside() {
        assert_eq!(stored_path("/infra/./ntp.md").unwrap(), "infra/ntp.md");
        assert_eq!(stored_path("AGENTS.md").unwrap(), "AGENTS.md");
        assert!(matches!(
            stored_path("/"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            stored_path("a/../../x.md"),
            Err(OkfError::InvalidPath { .. })
        ));
    }

    #[test]
    fn the_three_tools_are_listed_with_object_output_schemas() {
        let tools = OkfServer::tools().list_all();

        for name in ["discard", "revert", "pending_changes"] {
            let listed = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            let schema = listed.output_schema.as_ref().expect("an output schema");
            assert_eq!(schema["type"], "object", "{name}");
        }
    }
}

#[cfg(test)]
mod restructure_publish_tests {
    use super::*;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{app_with, changeset_row, open, TestApp};
    use crate::mcp::writes::{
        create_concept, edit_document, replace_body, CreateConceptParams, EditDocumentParams,
        ReplaceBodyParams,
    };
    use crate::okf::ops::{self, LogKind, Split, DEFAULT_SPLIT_TYPE};
    use crate::store::changesets::ChangesetState;
    use crate::store::history;

    const X: &str = "---\ntype: Note\ntitle: X\ndescription: X.\n---\n# X\n";
    const Y: &str = "---\ntype: Note\ntitle: Y\ndescription: Y.\n---\n# Y\n";
    const BIG: &str = "---\ntype: Note\ntitle: Big\ndescription: Big.\n---\n# Big\n\n\
                       ## Part\n\nPart text.\n\n### Deep\n\nDeep text.\n\n## Rest\n\nRest text.\n";
    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                       See [X](/infra/x.md) and [the part](/infra/big.md#part).\n";

    async fn pg() -> TestApp {
        let files: &[(&str, &str)] = &[
            (
                "index.md",
                "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
            ),
            (
                "infra/index.md",
                "# Infra\n\n# Concepts\n\n* [X](x.md) - x\n* [Y](y.md) - y\n* [Big](big.md) - big\n",
            ),
            ("infra/x.md", X),
            ("infra/y.md", Y),
            ("infra/big.md", BIG),
            ("hub.md", HUB),
            (
                "log.md",
                "# Log\n\n## 2026-11-01\n\n* **Update:** Added [X](/infra/x.md).\n",
            ),
        ];
        app_with(&[("pg", files)], Config::default()).await
    }

    /// A writer key's connection and the changeset it works in.
    struct Session {
        ctx: ToolContext,
        handle: String,
    }

    /// A `write` key on `pg` with one open changeset.
    async fn writer(test: &TestApp, key_id: &str, default: &str) -> Session {
        let ctx = crate::mcp::testing::writer(test, key_id, default).await;
        let handle = open(&ctx).await;
        Session { ctx, handle }
    }

    /// A `move_concept` write, through the write path as Task 6's tool makes it.
    async fn moved(session: &Session, old: &str, new: &str) -> WriteReport {
        let (old, new) = (old.to_string(), new.to_string());
        apply(&session.ctx, &session.handle, None, move |ws, _| {
            ops::move_concept(ws, &old, &new)
        })
        .await
        .unwrap()
    }

    /// A `delete_concept` write.
    async fn deleted(session: &Session, path: &str) -> WriteReport {
        let path = path.to_string();
        apply(&session.ctx, &session.handle, None, move |ws, _| {
            ops::delete_concept(ws, &path)
        })
        .await
        .unwrap()
    }

    /// A `log` write.
    async fn logged(session: &Session, text: &str) -> WriteReport {
        let text = text.to_string();
        apply(&session.ctx, &session.handle, None, move |ws, op| {
            ops::log(ws, op, LogKind::Update, &text)
        })
        .await
        .unwrap()
    }

    /// A `split_concept` write: `## Part` of `infra/big.md` (with its
    /// `### Deep`) to `infra/part.md`.
    async fn split_part(session: &Session) -> WriteReport {
        apply(&session.ctx, &session.handle, None, |ws, op| {
            let split = Split {
                concept: "infra/big.md",
                section: "Part",
                to: "infra/part.md",
                kind: DEFAULT_SPLIT_TYPE,
                title: None,
                description: Some("The part."),
                stub: None,
                sources: &[],
            };
            ops::split_concept(ws, op, &split)
        })
        .await
        .unwrap()
    }

    /// A new concept `Q` at `path` with `body`; listed in its folder when `listed`.
    async fn created(session: &Session, path: &str, body: &str, listed: bool) -> WriteReport {
        let frontmatter = serde_json::json!({"type": "Note", "title": "Q", "description": "Q."});
        let params = CreateConceptParams {
            path: path.to_string(),
            frontmatter: frontmatter.as_object().cloned().unwrap_or_default(),
            body: body.to_string(),
            // `bool::then`: `Some(..)` when true, like `x if cond else None`.
            index_summary: listed.then(|| "q".to_string()),
            index_section: None,
            by: None,
            changeset: session.handle.clone(),
        };
        create_concept(&session.ctx, params).await.unwrap()
    }

    async fn publish_as(session: &Session, message: &str) -> Result<Published, OkfError> {
        let params = PublishParams {
            message: message.to_string(),
            changeset: session.handle.clone(),
        };
        publish(&session.ctx, params).await
    }

    fn published_text(test: &TestApp, path: &str) -> String {
        let loaded = test.app.catalog("pg").unwrap().current();
        loaded
            .snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    /// Whether `path`'s `documents` row is a tombstone (`deleted_at` set).
    async fn tombstoned(test: &TestApp, path: &str) -> bool {
        // `$1` is a bind parameter; `documents` stores paths without the `/`.
        sqlx::query_scalar("SELECT deleted_at IS NOT NULL FROM documents WHERE path = $1")
            .bind(path)
            .fetch_one(test.app.pool())
            .await
            .unwrap()
    }

    /// The publish's revisions as sorted `(path, op, revision)`.
    fn revisions(published: &Published) -> Vec<(&str, &str, i32)> {
        let mut rows: Vec<(&str, &str, i32)> = published
            .report
            .revisions
            .iter()
            .map(|row| (row.path.as_str(), row.op, row.revision))
            .collect();
        rows.sort_unstable();
        rows
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_move_deletes_the_old_path_and_creates_the_new_one() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        moved(&ctx, "infra/x.md", "infra/z.md").await;

        let published = publish_as(&ctx, "move x").await.unwrap();

        assert_eq!(
            revisions(&published),
            [
                ("/hub.md", "update", 2),
                ("/infra/index.md", "update", 2),
                ("/infra/x.md", "delete", 2),
                ("/infra/z.md", "create", 1),
                ("/log.md", "update", 2),
            ]
        );
        assert!(published.report.replayed.is_empty());
        let old = history::revisions(test.app.pool(), state.id(), "infra/x.md", 5)
            .await
            .unwrap();
        let new = history::revisions(test.app.pool(), state.id(), "infra/z.md", 5)
            .await
            .unwrap();
        // Each path keeps its own history (plan Decision 16).
        assert_eq!((old.len(), old[0].op.as_str()), (2, "delete"));
        assert_eq!(old[0].message, "move x");
        assert_eq!((new.len(), new[0].op.as_str()), (1, "create"));
        assert!(tombstoned(&test, "infra/x.md").await);
        assert_eq!(published_text(&test, "infra/z.md"), X);
        assert!(published_text(&test, "hub.md").contains("[X](/infra/z.md)"));
        assert!(published_text(&test, "infra/index.md").contains("* [X](z.md) - x"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_replays_its_listing_onto_a_concurrent_index_of_the_folder() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        moved(&mover, "infra/y.md", "infra/w.md").await;
        created(&other, "/infra/q.md", "# Q\n", true).await;
        publish_as(&other, "add q").await.unwrap();

        let published = publish_as(&mover, "rename y").await.unwrap();

        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        let index = published_text(&test, "infra/index.md");
        assert!(index.contains("* [Y](w.md) - y"), "{index}");
        assert!(index.contains("* [Q](q.md) - q"), "{index}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_relinks_log_md_onto_a_concurrent_log() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        moved(&mover, "infra/x.md", "infra/z.md").await;
        logged(&other, "from b").await;
        publish_as(&other, "log b").await.unwrap();

        let published = publish_as(&mover, "move x").await.unwrap();

        assert_eq!(published.report.replayed, ["/log.md"]);
        let log = published_text(&test, "log.md");
        assert!(log.contains("* **Update:** from b"), "{log}");
        assert!(log.contains("Added [X](/infra/z.md)."), "{log}");
        assert!(!log.contains("/infra/x.md"), "{log}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_conflicts_with_a_concurrent_edit_of_a_linking_file() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = moved(&mover, "infra/x.md", "infra/z.md").await;
        let body = ReplaceBodyParams {
            path: "/hub.md".to_string(),
            body: "Nothing linked.\n".to_string(),
            changeset: other.handle.clone(),
        };
        replace_body(&other.ctx, body).await.unwrap();
        publish_as(&other, "edit hub").await.unwrap();

        let error = publish_as(&mover, "move x").await.unwrap_err();

        assert!(
            matches!(
                error,
                OkfError::Conflict { ref files } if files == &[ConflictFile {
                    path: "/hub.md".to_string(),
                    base_revision: Some(1),
                    current_revision: Some(2),
                }]
            ),
            "{error:?}"
        );
        let row = changeset_row(&test, &report.changeset).await;
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_to_the_old_path_published_meanwhile_refuses_the_move() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = moved(&mover, "infra/y.md", "infra/w.md").await;
        created(&other, "/b.md", "See [y](/infra/y.md).\n", false).await;
        publish_as(&other, "add b").await.unwrap();

        let error = publish_as(&mover, "rename y").await.unwrap_err();

        // `links_into_deleted` (store/publish.rs): b.md is not in the mover's
        // changeset and links to the path it deletes.
        let OkfError::LintFailed { errors, .. } = error else {
            panic!("expected LintFailed, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|finding| finding.file == "b.md" && finding.rule == "link-into-deleted"),
            "{errors:?}"
        );
        let row = changeset_row(&test, &report.changeset).await;
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_partial_discard_of_a_move_leaves_a_link_publish_refuses() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let report = moved(&ctx, "infra/x.md", "infra/z.md").await;
        // Drop only hub.md's rewrite: the published hub.md still links the old path.
        let params = DiscardParams {
            paths: Some(vec!["/hub.md".to_string()]),
            changeset: ctx.handle.clone(),
        };
        discard(&ctx.ctx, params).await.unwrap();

        let error = publish_as(&ctx, "rename x").await.unwrap_err();

        let OkfError::LintFailed { errors, .. } = error else {
            panic!("expected LintFailed, got {error:?}");
        };
        assert!(
            errors
                .iter()
                .any(|finding| finding.file == "hub.md" && finding.rule == "link-into-deleted"),
            "{errors:?}"
        );
        let row = changeset_row(&test, &report.changeset).await;
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_listing_changed_since_a_move_to_another_folder_refuses_the_publish() {
        let test = pg().await;
        let mover = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = moved(&mover, "infra/y.md", "ops/y.md").await;
        let edit = EditDocumentParams {
            path: "/infra/index.md".to_string(),
            old_str: "* [Y](y.md) - y".to_string(),
            new_str: "* [Y](y.md) - why".to_string(),
            replace_all: false,
            changeset: other.handle.clone(),
        };
        edit_document(&other.ctx, edit).await.unwrap();
        publish_as(&other, "reword y").await.unwrap();

        let error = publish_as(&mover, "move y").await.unwrap_err();

        assert!(
            matches!(error, OkfError::Refused { ref reason } if reason.contains("changed since")),
            "{error:?}"
        );
        let row = changeset_row(&test, &report.changeset).await;
        assert_eq!(row.state, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_delete_leaves_a_tombstone_and_unlists_onto_newer_text() {
        let test = pg().await;
        let deleter = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = deleted(&deleter, "infra/y.md").await;
        created(&other, "/infra/q.md", "# Q\n", true).await;
        publish_as(&other, "add q").await.unwrap();

        let published = publish_as(&deleter, "drop y").await.unwrap();

        assert_eq!(report.touched, ["/infra/y.md", "/infra/index.md"]);
        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        assert_eq!(
            revisions(&published),
            [
                ("/infra/index.md", "update", 3),
                ("/infra/y.md", "delete", 2)
            ]
        );
        let index = published_text(&test, "infra/index.md");
        assert!(
            !index.contains("[Y](y.md)") && index.contains("[Q](q.md)"),
            "{index}"
        );
        assert!(tombstoned(&test, "infra/y.md").await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_published_split_writes_the_parent_the_new_concept_and_its_listing() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        split_part(&ctx).await;

        let published = publish_as(&ctx, "split big").await.unwrap();

        assert_eq!(
            revisions(&published),
            [
                ("/hub.md", "update", 2),
                ("/infra/big.md", "update", 2),
                ("/infra/index.md", "update", 2),
                ("/infra/part.md", "create", 1),
            ]
        );
        assert!(published_text(&test, "infra/part.md").contains("Part text."));
        assert!(published_text(&test, "infra/big.md")
            .contains("## Part\n\n[Part](/infra/part.md): The part.\n"));
        assert!(published_text(&test, "hub.md").contains("[the part](/infra/part.md)"));
        assert!(published_text(&test, "infra/index.md").contains("* [Part](part.md) - The part."));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn links_into_a_split_section_published_meanwhile_resolve_or_only_warn() {
        let test = pg().await;
        let splitter = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        split_part(&splitter).await;
        created(&other, "/b.md", "See [deep](/infra/big.md#deep).\n", false).await;
        created(
            &other,
            "/c.md",
            "See [the part](/infra/big.md#part).\n",
            false,
        )
        .await;
        publish_as(&other, "add b and c").await.unwrap();

        let published = publish_as(&splitter, "split big").await.unwrap();

        // Plan Decision 18: split moves no path. `### Deep` moved to part.md,
        // so b.md's anchor is broken, in a file this publish does not change:
        // a warning. The parent keeps `## Part` (with its stub), so c.md's
        // link still resolves and has no finding at all.
        assert!(published.report.published);
        let warnings = &published.report.warnings;
        let broken: Vec<&str> = warnings
            .iter()
            .filter(|finding| finding.rule == "broken-anchor")
            .map(|finding| finding.file.as_str())
            .collect();
        assert_eq!(broken, ["b.md"], "{warnings:?}");
        assert!(
            !warnings.iter().any(|finding| finding.file == "c.md"),
            "{warnings:?}"
        );
        assert!(published_text(&test, "infra/big.md").contains("## Part\n"));
        assert!(published_text(&test, "infra/part.md").contains("Deep text."));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_unlists_a_listing_reworded_meanwhile() {
        let test = pg().await;
        let deleter = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        deleted(&deleter, "infra/y.md").await;
        let edit = EditDocumentParams {
            path: "/infra/index.md".to_string(),
            old_str: "* [Y](y.md) - y".to_string(),
            new_str: "* [Y](y.md) - why".to_string(),
            replace_all: false,
            changeset: other.handle.clone(),
        };
        edit_document(&other.ctx, edit).await.unwrap();
        publish_as(&other, "reword y").await.unwrap();

        let published = publish_as(&deleter, "drop y").await.unwrap();

        // `delete_concept` records `Unlist { bullet: None }` (plan Decision 6):
        // the bullet goes whatever it reads now, unlike a move's `Some`.
        assert_eq!(published.report.replayed, ["/infra/index.md"]);
        assert_eq!(
            revisions(&published),
            [
                ("/infra/index.md", "update", 3),
                ("/infra/y.md", "delete", 2)
            ]
        );
        let index = published_text(&test, "infra/index.md");
        assert!(!index.contains("y.md"), "{index}");
        assert!(index.contains("* [X](x.md) - x"), "{index}");
        assert!(tombstoned(&test, "infra/y.md").await);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_split_conflicts_with_a_concurrent_edit_of_its_parent() {
        let test = pg().await;
        let splitter = writer(&test, "writer-a", "pg").await;
        let other = writer(&test, "writer-b", "pg").await;
        let report = split_part(&splitter).await;
        let edit = EditDocumentParams {
            path: "/infra/big.md".to_string(),
            old_str: "Rest text.".to_string(),
            new_str: "Rest text, edited.".to_string(),
            replace_all: false,
            changeset: other.handle.clone(),
        };
        edit_document(&other.ctx, edit).await.unwrap();
        publish_as(&other, "edit big").await.unwrap();

        let error = publish_as(&splitter, "split big").await.unwrap_err();

        // The parent is a free edit (plan Decision 1), so a stale base conflicts.
        assert!(
            matches!(
                error,
                OkfError::Conflict { ref files } if files == &[ConflictFile {
                    path: "/infra/big.md".to_string(),
                    base_revision: Some(1),
                    current_revision: Some(2),
                }]
            ),
            "{error:?}"
        );
        let row = changeset_row(&test, &report.changeset).await;
        assert_eq!(row.state, ChangesetState::Open);
    }
}
