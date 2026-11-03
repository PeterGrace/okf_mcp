//! The changeset tools (design "Write tools"): `publish` (Task 10);
//! `discard`, `revert` and `pending_changes` (Task 11); `stranded_sessions`
//! and `adopt_changeset` (Task 12). Each task's tools are one
//! `#[tool_router]` block with its own router, joined in `OkfServer::tools`.

use std::sync::Arc;
use std::time::Duration;

use axum::http::request::Parts;
use chrono::{DateTime, TimeDelta, Utc};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogState;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::store::keys::Access;

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
    let changeset = changesets::find_open(ctx.app().pool(), &key).await?;
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
    /// The changeset published, if this connection had one open.
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
