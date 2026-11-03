//! The changeset tools (design "Write tools"): `publish` (Task 10);
//! `discard`, `revert` and `pending_changes` (Task 11); `stranded_sessions`
//! and `adopt_changeset` (Task 12). Each task's tools are one
//! `#[tool_router]` block with its own router, joined in `OkfServer::tools`.

use std::collections::BTreeMap;
use std::sync::Arc;
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

use crate::catalog::{CatalogState, Loaded};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::reports::CatalogParam;
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::ops::OpOutcome;
use crate::okf::paths::{escapes_root, normpath};
use crate::okf::rules;
use crate::okf::workspace::Workspace;
use crate::store::keys::Access;
use crate::write::{apply, WriteReport};

/// `stranded_sessions`' arguments.
// `Deserialize` reads them from the call's JSON; `JsonSchema` gives the tool
// its input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct StrandedParams {
    /// Only this catalog (default: every catalog the key reaches).
    pub catalog: Option<String>,
}

/// One stranded changeset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StrandedChangeset {
    /// Its id: what `adopt_changeset` takes.
    pub id: i64,
    /// Its catalog.
    pub catalog: String,
    /// The label of the connection that made it.
    pub label: String,
    /// The key it belongs to; null once that key is deleted.
    pub key: Option<String>,
    /// The actor of the key that opened it.
    pub actor: String,
    /// When it was opened.
    pub created: String,
    /// Its last change.
    pub last_change: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// `stranded_sessions`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Stranded {
    /// By catalog, oldest change first.
    pub changesets: Vec<StrandedChangeset>,
}

/// `adopt_changeset`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct AdoptParams {
    /// The changeset's id, from `stranded_sessions`.
    pub id: i64,
}

/// `adopt_changeset`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Adopted {
    /// The changeset's id.
    pub id: i64,
    /// Its catalog.
    pub catalog: String,
    /// Its label now: this connection's.
    pub label: String,
    /// The key it belonged to.
    pub previous_key: Option<String>,
    /// The label it had.
    pub previous_label: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// The time before which an open changeset's last change makes it stranded.
///
/// # Arguments
/// * `now` - the current time (server clock).
/// * `idle` - `session_idle_timeout`.
///
/// # Returns
/// `now - idle`, or the earliest representable time if that would overflow
/// (an absurdly long timeout: nothing is stranded).
fn idle_cutoff(now: DateTime<Utc>, idle: Duration) -> DateTime<Utc> {
    // `TimeDelta::from_std` fails for a duration beyond chrono's range, and
    // `checked_sub_signed` for a result before its earliest date; both `ok()`
    // and `and_then` turn a failure into `None`.
    TimeDelta::from_std(idle)
        .ok()
        .and_then(|idle| now.checked_sub_signed(idle))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

/// Lists stranded changesets (design "Session lifecycle").
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - an optional catalog to narrow the list to.
///
/// # Returns
/// Open, non-empty changesets not changed within `session_idle_timeout`,
/// in catalogs the key reaches: the key's own (except this connection's
/// own, same label), and with `admin` every key's.
///
/// # Errors
/// A selection error for `catalog`; [`OkfError::Database`].
pub(crate) async fn stranded_sessions(
    ctx: &ToolContext,
    params: StrandedParams,
) -> Result<Stranded, OkfError> {
    // Local: Tasks 10 and 11 import the store in this file in their own way.
    use crate::store::changesets;

    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>`, so
    // `?` can fail on an unreachable catalog named in the argument.
    let only = params
        .catalog
        .as_deref()
        .map(|name| ctx.one(Some(name)))
        .transpose()?
        .map(|state| state.id());
    let now = ctx.app().clock().now();
    let idle_since = idle_cutoff(now, ctx.app().config().session_idle_timeout);
    let principal = ctx.principal();
    let admin = principal.access >= Access::Admin;
    // The database narrows by key (unless admin) and catalog, so other
    // keys' paths never leave it; reach and this connection are left out here.
    let key_filter = (!admin).then_some(principal.key_id.as_str());
    let rows = changesets::stranded(ctx.app().pool(), idle_since, key_filter, only).await?;
    let changesets = rows
        .into_iter()
        .filter(|row| {
            let own = row.changeset.key_id.as_deref() == Some(principal.key_id.as_str());
            let this_connection = own && row.changeset.label == ctx.label();
            principal.reaches(&row.catalog) && !this_connection
        })
        .map(|row| StrandedChangeset {
            id: row.changeset.id,
            catalog: row.catalog,
            label: row.changeset.label,
            key: row.changeset.key_id,
            actor: row.changeset.actor,
            created: format_timestamp(row.changeset.created_at),
            last_change: format_timestamp(row.changeset.updated_at),
            paths: row.paths,
        })
        .collect();
    Ok(Stranded { changesets })
}

/// Moves an open changeset to this connection's key and label.
///
/// The changeset is read once to find its catalog. Then, in one
/// transaction and in the lock order of Tasks 1 and 8, the catalog row is
/// locked, then the changeset's row, which is read again under that lock.
/// The access check and this connection's open-changeset lookup use that
/// locked row, so a concurrent adoption, publish, discard or prune either
/// finishes first (and is seen) or waits for this one.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the changeset's id.
///
/// # Returns
/// The changeset's catalog, new and old label and key, and its paths.
///
/// # Errors
/// [`OkfError::Refused`] if there is no open changeset with that id in a
/// catalog the key reaches (one message for every such case), or if this
/// connection already has another open changeset in that catalog;
/// [`OkfError::Forbidden`] without `write` (the key's own) or `admin`
/// (another key's); [`OkfError::Busy`] on a deadlock;
/// [`OkfError::Database`].
pub(crate) async fn adopt_changeset(
    ctx: &ToolContext,
    params: AdoptParams,
) -> Result<Adopted, OkfError> {
    use crate::store::changesets::{self, ChangesetKey, ChangesetState};

    let id = params.id;
    // One message for "no such id", "closed" and "not in your catalogs", so
    // a key cannot tell which (and so probe other catalogs).
    let unknown = || OkfError::Refused {
        reason: format!("no open changeset {id} in a catalog this key reaches"),
    };
    // Used twice with `Option::filter`, which keeps `Some(row)` only when
    // this returns `true`.
    let is_open = |row: &changesets::ChangesetRow| row.state == ChangesetState::Open;
    // A changeset never moves between catalogs, so this unlocked read is
    // safe for the catalog id; everything else is read again under the lock.
    let catalog_id = changesets::by_id(ctx.app().pool(), id)
        .await?
        .filter(is_open)
        .ok_or_else(unknown)?
        .catalog_id;
    let principal = ctx.principal();
    let state = ctx
        .catalogs()
        .values()
        .find(|state| state.id() == catalog_id && principal.reaches(state.name()))
        .ok_or_else(unknown)?;

    let now = ctx.app().clock().now();
    // An early `return` or `?` below drops `tx` uncommitted, which rolls it
    // back (Rust runs the transaction's `Drop` on every exit path).
    let mut tx = ctx.app().pool().begin().await?;
    changesets::lock_catalog(&mut tx, catalog_id).await?;
    let row = changesets::lock(&mut tx, id)
        .await?
        .filter(is_open)
        .ok_or_else(unknown)?;
    let own = row.key_id.as_deref() == Some(principal.key_id.as_str());
    let needed = if own { Access::Write } else { Access::Admin };
    principal.require(needed, state.name())?;
    let mine = ChangesetKey {
        catalog_id,
        key_id: &principal.key_id,
        label: ctx.label(),
    };
    if let Some(open) = changesets::find_open(&mut *tx, &mine).await? {
        // Adopting the changeset this connection already has is a no-op
        // relabel; any other open one is in the way.
        if open.id != id {
            return Err(OkfError::Refused {
                reason: format!(
                    "this connection already has open changeset {} in {}; publish or \
                     discard it first, or connect with another X-OKF-Changeset label",
                    open.id,
                    state.name()
                ),
            });
        }
    }
    let adopted = changesets::adopt(&mut tx, id, &principal.key_id, ctx.label(), now).await?;
    let entries = changesets::entries(&mut *tx, id).await?;
    tx.commit().await?;
    Ok(Adopted {
        id,
        catalog: state.name().to_string(),
        label: adopted.label,
        previous_key: row.key_id,
        previous_label: row.label,
        paths: entries.into_iter().map(|entry| entry.path).collect(),
    })
}

// As in the read tool modules: the macro turns this block's `#[tool]`
// methods into `lifecycle_router`, which `OkfServer::tools` joins.
#[tool_router(router = lifecycle_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `stranded_sessions` tool.
    #[tool(
        description = "Open changesets holding unpublished edits that nobody has changed \
                       within session_idle_timeout: this key's own (from other connections or \
                       labels), or with an admin key every one in the catalogs it reaches. Each \
                       has an id (for adopt_changeset), catalog, label, key, actor, creation and \
                       last-change times, and changed paths."
    )]
    async fn stranded_sessions(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<StrandedParams>,
    ) -> Result<Json<Stranded>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(stranded_sessions(&ctx, params).await)
    }

    /// The `adopt_changeset` tool.
    #[tool(
        description = "Move an open changeset (its id from stranded_sessions) to this \
                       connection, so this connection's reads show it and its writes, publish \
                       and discard act on it. A write key adopts its own changesets; another \
                       key's needs an admin key. Refused while this connection already has an \
                       open changeset in that catalog."
    )]
    async fn adopt_changeset(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<AdoptParams>,
    ) -> Result<Json<Adopted>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(adopt_changeset(&ctx, params).await)
    }
}

// ---------------------------------------------------------------------------
// publish (Task 10)
// ---------------------------------------------------------------------------

/// The catalog a changeset tool acts on, and this connection's open
/// changeset there (if any).
#[derive(Debug)]
struct OwnChangeset {
    state: Arc<CatalogState>,
    changeset: Option<crate::store::changesets::ChangesetRow>,
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
    // Local, as in Task 12's functions: each task imports the store its own way.
    use crate::store::changesets::{self, ChangesetKey};

    let state = ctx.one(catalog)?;
    let principal = ctx.principal();
    principal.require(access, state.name())?;
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &principal.key_id,
        label: ctx.label(),
    };
    // As in `ToolContext::view`: a key below `write` never has a changeset
    // to read, so it skips the lookup.
    let changeset = if principal.access < Access::Write {
        None
    } else {
        changesets::find_open(ctx.app().pool(), &key).await?
    };
    Ok(OwnChangeset { state, changeset })
}

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
    /// This connection's open changeset, if it had one. With `published: false` it was not published and stays open (it held no entries, for instance after a discard emptied it).
    pub changeset: Option<i64>,
    /// What was written; `flatten` puts its fields beside these.
    #[serde(flatten)]
    pub report: crate::store::publish::PublishReport,
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
/// `write`); as [`crate::store::publish::publish`].
pub(crate) async fn publish(
    ctx: &ToolContext,
    params: PublishParams,
) -> Result<Published, OkfError> {
    // `as publishing`: the module's `publish` would clash with this function.
    use crate::store::publish::{self as publishing, PublishReport};

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
                       then stays open. Lint errors elsewhere come back as warnings. When every \
                       edit already matches the published text, the changeset is closed with \
                       published: true and no revisions. Write access."
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
    /// The paths to drop, e.g. `["/infra/ntp.md"]` (default: every entry, which also closes the changeset).
    pub paths: Option<Vec<String>>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `discard`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Discarded {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The changeset, if this connection had one open.
    pub changeset: Option<i64>,
    /// How many entries were dropped.
    pub removed: u64,
    /// Whether the changeset was closed (`discarded`): no `paths` given. The next write opens a new one.
    pub closed: bool,
}

/// `revert`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct RevertParams {
    /// The document as stored, with its extension, e.g. `/infra/ntp.md`.
    pub path: String,
    /// The revision to go back to, as `history` lists it; a `delete` revision means deleting the document.
    pub revision: i32,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
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
    /// A unified diff against the published text (`/dev/null` for a new or deleted file).
    pub diff: String,
}

/// `pending_changes`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct PendingChanges {
    /// The catalog.
    pub catalog: String,
    /// This connection's changeset label.
    pub label: String,
    /// The open changeset, if any.
    pub changeset: Option<i64>,
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

/// Drops entries from this connection's changeset; without `paths`, all
/// of them, and the changeset is closed (`discarded`).
///
/// Takes publish's locks in publish's order: the catalog's in-process
/// publish lock, then (in the transaction) the advisory lock, the catalog
/// row and the changeset row. So a discard and a publish of one changeset
/// run one after the other, and a write queued behind a closing discard
/// opens a new changeset.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the paths and catalog.
///
/// # Returns
/// How many entries went, and whether the changeset was closed.
///
/// # Errors
/// As [`own_changeset`] (with `write`); [`OkfError::InvalidPath`] for a
/// path outside the catalog; [`OkfError::Refused`] if a publish or another
/// discard closed the changeset meanwhile; [`OkfError::Database`].
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
    let OwnChangeset { state, changeset } =
        own_changeset(ctx, params.catalog.as_deref(), Access::Write).await?;
    let mut discarded = Discarded {
        catalog: state.name().to_string(),
        label: ctx.label().to_string(),
        changeset: None,
        removed: 0,
        closed: false,
    };
    let Some(changeset) = changeset else {
        return Ok(discarded);
    };
    // An empty list drops nothing: no lock, no version bump, and the cached
    // overlay stays valid.
    if paths.as_ref().is_some_and(Vec::is_empty) {
        discarded.changeset = Some(changeset.id);
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
            reason: format!("changeset {} is no longer open", changeset.id),
        })?;
    discarded.removed =
        changesets::remove_entries(&mut tx, changeset.id, paths.as_deref(), now).await?;
    discarded.closed = paths.is_none();
    if discarded.closed {
        changesets::set_state(&mut tx, changeset.id, ChangesetState::Discarded, now).await?;
    }
    tx.commit().await?;
    app.overlays().forget(changeset.id);
    discarded.changeset = Some(changeset.id);
    Ok(discarded)
}

/// Adds a document's content at `revision` (or its deletion, for a
/// `delete` revision) to this connection's changeset, as a free edit.
///
/// It goes through [`apply`], so every write rule applies. Sending a path
/// the changeset created (never published) back to "absent" leaves no
/// entry for it, so the report's `touched` is empty.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the path, revision and catalog.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// [`OkfError::InvalidPath`] for machinery or a path outside the catalog
/// (before any history is read); a selection error;
/// [`OkfError::Forbidden`] below `write`; [`OkfError::NotFound`] for a
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
        catalog,
    } = params;
    // The write rules refuse machinery here, before the history is read;
    // `apply` checks the touched path again.
    let path = rules::check_path(&path)?;
    let state = ctx.one(catalog.as_deref())?;
    ctx.principal().require(Access::Write, state.name())?;
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
    apply(ctx, Some(state.name()), None, move |ws, _| {
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

/// This connection's changeset in a catalog, as diffs against the published
/// text.
///
/// It sees what [`ToolContext::view`] would: a key below `write` has no
/// changeset (the list is empty), else the open changeset of this key and
/// label. It reads the entries and the published state rather than
/// building the overlay, so a changeset whose overlay no longer applies
/// can still be listed, and then discarded.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog.
///
/// # Returns
/// The changed paths and their diffs; empty without an open changeset.
///
/// # Errors
/// As [`own_changeset`] (any key that reaches the catalog may ask).
pub(crate) async fn pending_changes(
    ctx: &ToolContext,
    params: CatalogParam,
) -> Result<PendingChanges, OkfError> {
    use crate::store::changesets;

    let OwnChangeset { state, changeset } =
        own_changeset(ctx, params.catalog.as_deref(), Access::Read).await?;
    let files = match &changeset {
        None => Vec::new(),
        Some(row) => {
            let entries = changesets::entries(ctx.app().pool(), row.id).await?;
            let loaded = state.current();
            blocking(move || pending_files(&loaded, &entries)).await
        }
    };
    Ok(PendingChanges {
        catalog: state.name().to_string(),
        label: ctx.label().to_string(),
        changeset: changeset.map(|row| row.id),
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
/// not UTF-8.
fn unified_diff(path: &str, published: Option<&[u8]>, pending: Option<&[u8]>) -> String {
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
        description = "Drop entries from this connection's changeset in a catalog: the named \
                       `paths`, or every entry (which also closes the changeset; the next write \
                       opens a new one). Published documents are not touched. Write access."
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
        description = "Add a document's content at an earlier revision (from `history`), or its \
                       deletion for a `delete` revision, to this connection's changeset, for \
                       review and publish like any other edit. Every write rule applies, so \
                       machinery cannot be reverted. Reverting a file this changeset created to \
                       a `delete` revision drops it from the changeset. Write access."
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
        description = "This connection's unpublished changeset in a catalog: each changed path \
                       (create, update or delete), the revision it was based on and the one \
                       published now, whether that base is stale, whether publish can replay \
                       it, and a unified diff against the published text."
    )]
    async fn pending_changes(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> Result<Json<PendingChanges>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(pending_changes(&ctx, params).await)
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use chrono::{TimeDelta, Utc};

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, pending_entry, principal_as, store_key, PendingEdit, TestApp,
    };
    use crate::store::changesets::{self, ChangesetKey, ChangesetState};
    use crate::store::keys::Access;

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

    /// A connection of `key` with `access` on `label`, reaching `reach`.
    fn connection(
        test: &TestApp,
        key: &str,
        access: Access,
        reach: &[&str],
        label: &str,
    ) -> ToolContext {
        context(test, principal_as(key, access, reach, Some("pg")), None).with_label(label)
    }

    /// An edit of `a.md` in `pg`, `hours_ago` hours ago, by `ctx`.
    async fn edit_hours_ago(test: &TestApp, ctx: &ToolContext, hours_ago: i64) -> i64 {
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md",
            text: Some("# Pending\n"),
            at: Utc::now() - TimeDelta::hours(hours_ago),
        };
        pending_entry(test, ctx, &edit).await.id
    }

    fn ids(stranded: &Stranded) -> Vec<i64> {
        stranded
            .changesets
            .iter()
            .map(|changeset| changeset.id)
            .collect()
    }

    fn all() -> StrandedParams {
        StrandedParams { catalog: None }
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
    async fn stranded_lists_the_key_s_idle_changesets_and_all_of_them_for_an_admin() {
        let test = test_app().await;
        let alice_old = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "fresh"),
            0,
        )
        .await;
        let bob_old = edit_hours_ago(
            &test,
            &connection(&test, "bob", Access::Write, &[], "old"),
            3,
        )
        .await;

        let alice = stranded_sessions(&connection(&test, "alice", Access::Write, &[], "x"), all())
            .await
            .unwrap();
        let alice_on_old = stranded_sessions(
            &connection(&test, "alice", Access::Write, &[], "old"),
            all(),
        )
        .await
        .unwrap();
        let admin = stranded_sessions(&connection(&test, "root", Access::Admin, &[], "x"), all())
            .await
            .unwrap();
        let admin_of_work = stranded_sessions(
            &connection(&test, "root", Access::Admin, &["work"], "x"),
            all(),
        )
        .await
        .unwrap();
        let narrowed = stranded_sessions(
            &connection(&test, "root", Access::Admin, &[], "x"),
            StrandedParams {
                catalog: Some("work".to_string()),
            },
        )
        .await
        .unwrap();
        let narrowed_to_pg = stranded_sessions(
            &connection(&test, "root", Access::Admin, &[], "x"),
            StrandedParams {
                catalog: Some("pg".to_string()),
            },
        )
        .await
        .unwrap();

        assert_eq!(ids(&alice), [alice_old]);
        let listed = &alice.changesets[0];
        assert_eq!(
            (
                listed.catalog.as_str(),
                listed.label.as_str(),
                listed.key.as_deref()
            ),
            ("pg", "old", Some("alice"))
        );
        assert_eq!(listed.paths, ["a.md"]);
        assert!(
            ids(&alice_on_old).is_empty(),
            "this connection's own is not stranded"
        );
        let mut both = ids(&admin);
        both.sort_unstable();
        let mut expected = vec![alice_old, bob_old];
        expected.sort_unstable();
        assert_eq!(both, expected);
        assert!(ids(&admin_of_work).is_empty());
        assert!(ids(&narrowed).is_empty());
        let mut in_pg = ids(&narrowed_to_pg);
        in_pg.sort_unstable();
        assert_eq!(in_pg, expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_moves_a_changeset_to_this_connection() {
        let test = test_app().await;
        let stranded_id = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        let here = connection(&test, "alice", Access::Write, &[], "default");

        let adopted = adopt_changeset(&here, AdoptParams { id: stranded_id })
            .await
            .unwrap();
        let after = stranded_sessions(&here, all()).await.unwrap();

        assert_eq!(adopted.id, stranded_id);
        assert_eq!(
            (
                adopted.catalog.as_str(),
                adopted.label.as_str(),
                adopted.previous_label.as_str()
            ),
            ("pg", "default", "old")
        );
        assert_eq!(adopted.paths, ["a.md"]);
        assert!(
            after.changesets.is_empty(),
            "adopted means changed just now"
        );
        let pg = test.app.catalog("pg").unwrap();
        let key = ChangesetKey {
            catalog_id: pg.id(),
            key_id: "alice",
            label: "default",
        };
        let open = changesets::find_open(test.app.pool(), &key).await.unwrap();
        assert_eq!(open.map(|row| row.id), Some(stranded_id));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_key_s_changeset_needs_admin_to_adopt() {
        let test = test_app().await;
        let bobs = edit_hours_ago(
            &test,
            &connection(&test, "bob", Access::Write, &[], "old"),
            3,
        )
        .await;

        let by_writer = adopt_changeset(
            &connection(&test, "alice", Access::Write, &[], "default"),
            AdoptParams { id: bobs },
        )
        .await;
        let by_admin = adopt_changeset(
            &connection(&test, "root", Access::Admin, &[], "default"),
            AdoptParams { id: bobs },
        )
        .await;

        assert!(matches!(by_writer, Err(OkfError::Forbidden { .. })));
        let adopted = by_admin.unwrap();
        assert_eq!(adopted.previous_key.as_deref(), Some("bob"));
        let row = changesets::by_id(test.app.pool(), bobs)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.key_id.as_deref(), Some("root"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_is_refused_for_a_busy_connection_or_an_unknown_id() {
        let test = test_app().await;
        let stranded_id = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "old"),
            3,
        )
        .await;
        let here = connection(&test, "alice", Access::Write, &[], "default");
        edit_hours_ago(&test, &here, 0).await;
        let closed_id = edit_hours_ago(
            &test,
            &connection(&test, "alice", Access::Write, &[], "closed"),
            3,
        )
        .await;
        let pg_id = test.app.catalog("pg").unwrap().id();
        let mut tx = test.app.pool().begin().await.unwrap();
        changesets::lock_catalog(&mut tx, pg_id).await.unwrap();
        changesets::set_state(&mut tx, closed_id, ChangesetState::Discarded, Utc::now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let busy = adopt_changeset(&here, AdoptParams { id: stranded_id }).await;
        let unknown = adopt_changeset(&here, AdoptParams { id: i64::MAX }).await;
        let elsewhere = connection(&test, "alice", Access::Write, &[], "elsewhere");
        let closed = adopt_changeset(&elsewhere, AdoptParams { id: closed_id }).await;
        let unreachable = adopt_changeset(
            &connection(&test, "alice", Access::Write, &["work"], "default"),
            AdoptParams { id: stranded_id },
        )
        .await;

        assert!(
            matches!(&busy, Err(OkfError::Refused { reason }) if reason.contains("already has")),
            "{busy:?}"
        );
        for refused in [unknown, closed, unreachable] {
            assert!(
                matches!(&refused, Err(OkfError::Refused { reason }) if reason.contains("no open changeset")),
                "{refused:?}"
            );
        }
    }

    #[test]
    fn both_tools_are_listed_with_object_output_schemas() {
        let tools = OkfServer::tools().list_all();

        for name in ["stranded_sessions", "adopt_changeset"] {
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
mod publish_tests {
    use super::*;
    use std::time::Duration;

    use chrono::Utc;

    use crate::config::Config;
    use crate::error::ConflictFile;
    use crate::mcp::testing::{app_with, context, files, principal_as, writer, TestApp};
    use crate::mcp::writes::{replace_body, ReplaceBodyParams};
    use crate::okf::ops::{self, LogKind};
    use crate::store::changesets::{self, ChangesetKey, ChangesetState, EntryRow};
    use crate::store::import::{import, ImportOptions};
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
        insert_entry(&test, "writer-a", new_file).await;
        insert_entry(&test, "writer-a", deletion).await;

        let first = publish_as(&ctx, "add b, drop y").await.unwrap();
        // A tombstone: the number moved on, the content and the snapshot entry are gone.
        let tombstone = document_row(&test, "infra/y.md").await;
        let gone = test.app.catalog("pg").unwrap().current();
        assert_eq!(tombstone, (2, true, Vec::new()));
        assert!(gone.snapshot().get("infra/y.md").is_none());
        // The changeset is closed, so this opens a new one: y again, on its tombstone.
        let again = EntryRow {
            path: "infra/y.md".to_string(),
            base_revision: None,
            content: Some(Y.as_bytes().to_vec()),
            executable: false,
            replay: None,
        };
        insert_entry(&test, "writer-a", again).await;
        let second = publish_as(&ctx, "bring y back").await.unwrap();

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
        assert_eq!(deleted, (None, first.changeset));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revision_written_meanwhile_is_a_conflict_not_a_database_error() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        let written = body(&ctx, "/a.md", "# A\n\nMine.\n").await;
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

        let error = publish_as(&ctx, "mine").await.unwrap_err();

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
        assert_eq!(
            state_of(&test, written.changeset).await,
            ChangesetState::Open
        );
        assert_eq!(document_row(&test, "a.md").await.0, 1);
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

#[cfg(test)]
mod changes_tests {
    use super::*;
    use std::time::Duration;

    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, files, pending_entry, principal, principal_as, writer, PendingEdit,
        TestApp,
    };
    use crate::mcp::writes::{
        create_concept, replace_body, CreateConceptParams, ReplaceBodyParams,
    };
    use crate::store::changesets::{self, ChangesetState};
    use crate::store::import::{import, ImportOptions};

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

    async fn body(ctx: &ToolContext, path: &str, text: &str) -> WriteReport {
        let params = ReplaceBodyParams {
            path: path.to_string(),
            body: text.to_string(),
            catalog: None,
        };
        replace_body(ctx, params).await.unwrap()
    }

    async fn new_note(ctx: &ToolContext, path: &str) -> WriteReport {
        let frontmatter =
            serde_json::json!({"type": "Note", "title": "New", "description": "New."});
        let params = CreateConceptParams {
            path: path.to_string(),
            frontmatter: frontmatter.as_object().unwrap().clone(),
            body: "# New\n".to_string(),
            index_summary: None,
            index_section: None,
            by: None,
            catalog: None,
        };
        create_concept(ctx, params).await.unwrap()
    }

    fn revert_to(path: &str, revision: i32) -> RevertParams {
        RevertParams {
            path: path.to_string(),
            revision,
            catalog: None,
        }
    }

    fn discard_paths(paths: Option<&[&str]>) -> DiscardParams {
        let paths = paths.map(|paths| paths.iter().map(|path| path.to_string()).collect());
        DiscardParams {
            paths,
            catalog: None,
        }
    }

    async fn pending(ctx: &ToolContext) -> PendingChanges {
        pending_changes(ctx, CatalogParam { catalog: None })
            .await
            .unwrap()
    }

    async fn state_of(test: &TestApp, changeset: i64) -> ChangesetState {
        changesets::by_id(test.app.pool(), changeset)
            .await
            .unwrap()
            .unwrap()
            .state
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
        let written = body(&ctx, "/a.md", "One.\n").await;
        body(&ctx, "/b.md", "Two.\n").await;
        let version = |test: &TestApp| {
            let pool = test.app.pool().clone();
            async move {
                changesets::by_id(&pool, written.changeset)
                    .await
                    .unwrap()
                    .unwrap()
                    .version
            }
        };
        let before = version(&test).await;

        let empty = discard(&ctx, discard_paths(Some(&[]))).await.unwrap();
        let unchanged = version(&test).await;
        let some = discard(&ctx, discard_paths(Some(&["/a.md", "/c.md"])))
            .await
            .unwrap();
        let left = pending(&ctx).await;
        let all = discard(&ctx, discard_paths(None)).await.unwrap();
        let after = pending(&ctx).await;
        let nothing = discard(&ctx, discard_paths(None)).await.unwrap();
        let outside = discard(&ctx, discard_paths(Some(&["/../x.md"])))
            .await
            .unwrap_err();
        let again = body(&ctx, "/a.md", "Three.\n").await;

        // An empty list drops nothing and leaves the version alone.
        assert_eq!(
            (empty.changeset, empty.removed, empty.closed),
            (Some(written.changeset), 0, false)
        );
        assert_eq!(unchanged, before);
        assert_eq!(
            (some.changeset, some.removed, some.closed),
            (Some(written.changeset), 1, false)
        );
        let paths: Vec<&str> = left.files.iter().map(|file| file.path.as_str()).collect();
        assert_eq!(paths, ["/b.md"]);
        assert_eq!((all.removed, all.closed), (1, true));
        assert_eq!(
            state_of(&test, written.changeset).await,
            ChangesetState::Discarded
        );
        assert_eq!((after.changeset, after.files.len()), (None, 0));
        assert_eq!(
            (nothing.changeset, nothing.removed, nothing.closed),
            (None, 0, false)
        );
        assert_eq!(outside.kind(), "InvalidPath");
        assert_ne!(
            again.changeset, written.changeset,
            "a write after a discard opens a new changeset"
        );
        // The published documents are untouched.
        let loaded = test.app.catalog("pg").unwrap().current();
        assert_eq!(
            loaded.snapshot().get("a.md").map(|doc| doc.text()),
            Some(NOTE)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn discard_needs_write_access() {
        let test = pg().await;
        let reader = context(&test, principal(&[], Some("pg")), None);

        let refused = discard(&reader, discard_paths(None)).await.unwrap_err();

        assert_eq!(refused.kind(), "Forbidden");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_queued_behind_a_discard_lands_in_a_new_changeset() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        let opened = body(&ctx, "/a.md", "First.\n").await;
        // Hold the changeset's row lock, so the discard and then the write
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
        let discarder = same_key();
        let discarding =
            tokio::spawn(async move { discard(&discarder, discard_paths(None)).await });
        lock_waiters(&test, 1).await;
        let late = same_key();
        let writing = tokio::spawn(async move { body(&late, "/b.md", "Late.\n").await });
        lock_waiters(&test, 2).await;

        holder.rollback().await.unwrap();
        let discarded = discarding.await.unwrap().unwrap();
        let written = writing.await.unwrap();

        // The discard took the lock first and closed the changeset. The
        // write's `get_or_create` re-checks `state = 'open'` once the lock is
        // granted, finds none, and opens a new changeset: never `Busy`.
        assert_eq!(
            (discarded.changeset, discarded.removed, discarded.closed),
            (Some(opened.changeset), 1, true)
        );
        assert_eq!(
            state_of(&test, opened.changeset).await,
            ChangesetState::Discarded
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
        assert_eq!(paths, ["b.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revert_writes_an_old_revision_as_a_free_edit() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        republish(
            &test,
            &[
                ("a.md", "# Two\n"),
                ("b.md", NOTE_B),
                ("AGENTS.md", "# Agents\n"),
            ],
        )
        .await;

        let reverted = revert(&ctx, revert_to("/a.md", 1)).await.unwrap();
        let shown = pending(&ctx).await;
        republish(
            &test,
            &[
                ("a.md", "# Three\n"),
                ("b.md", NOTE_B),
                ("AGENTS.md", "# Agents\n"),
            ],
        )
        .await;
        let stale = pending(&ctx).await;

        assert_eq!(reverted.touched, ["/a.md"]);
        assert_eq!(reverted.message, "a.md: reverted to revision 1");
        let entries = changesets::entries(test.app.pool(), reverted.changeset)
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

        let deleted = revert(&ctx, revert_to("/b.md", 2)).await.unwrap();
        let twice = revert(&ctx, revert_to("/b.md", 2)).await.unwrap_err();
        let machinery = revert(&ctx, revert_to("/AGENTS.md", 1)).await.unwrap_err();
        let no_such = revert(&ctx, revert_to("/a.md", 9)).await.unwrap_err();
        // A revision whose bytes are not UTF-8 (no write tool stores one).
        sqlx::query("UPDATE revisions SET content = '\\xff'::bytea WHERE path = 'a.md'")
            .execute(test.app.pool())
            .await
            .unwrap();
        let binary = revert(&ctx, revert_to("/a.md", 1)).await.unwrap_err();
        let reader = context(&test, principal(&[], Some("pg")), None);
        let read_only = revert(&reader, revert_to("/a.md", 1)).await.unwrap_err();

        assert_eq!(deleted.touched, ["/b.md"]);
        let entries = changesets::entries(test.app.pool(), deleted.changeset)
            .await
            .unwrap();
        assert_eq!(
            (entries[0].path.as_str(), entries[0].content.as_ref()),
            ("b.md", None)
        );
        assert_eq!(entries[0].base_revision, Some(3));
        let shown = pending(&ctx).await;
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
        // b.md: 1 imported, 2 deleted. The changeset then creates it anew.
        republish(&test, &[("a.md", NOTE), ("AGENTS.md", "# Agents\n")]).await;
        let created = new_note(&ctx, "/b.md").await;

        let reverted = revert(&ctx, revert_to("/b.md", 2)).await.unwrap();

        assert_eq!(created.touched, ["/b.md"]);
        // Never published as the changeset made it and not published now: the
        // changeset's own entry goes, so nothing is left to list.
        assert!(reverted.touched.is_empty(), "{:?}", reverted.touched);
        assert_eq!(reverted.changeset, created.changeset);
        assert!(changesets::entries(test.app.pool(), created.changeset)
            .await
            .unwrap()
            .is_empty());
        assert!(pending(&ctx).await.files.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pending_changes_diffs_a_new_file_against_dev_null_and_needs_only_read() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        new_note(&ctx, "/new.md").await;

        let shown = pending(&ctx).await;
        let reader = pending(&context(&test, principal(&[], Some("pg")), None)).await;

        assert_eq!(
            (shown.catalog.as_str(), shown.label.as_str()),
            ("pg", "default")
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
        assert!(reader.files.is_empty() && reader.changeset.is_none());
        assert_eq!(reader.catalog, "pg");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_changeset_whose_overlay_no_longer_applies_can_be_listed_and_discarded() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let state = test.app.catalog("pg").unwrap();
        // `a.md` is a published file, so an entry under it can never apply
        // (no write tool stores one; a publish made after the edit can).
        let edit = PendingEdit {
            catalog: "pg",
            path: "a.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &ctx, &edit).await;

        let broken = ctx.view(&state).await.err();
        let shown = pending(&ctx).await;
        let dropped = discard(&ctx, discard_paths(Some(&["/a.md/x.md"])))
            .await
            .unwrap();
        let fixed = ctx.view(&state).await;

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
