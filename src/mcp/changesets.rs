//! The changeset tools (design "Write tools"): `publish` (Task 10);
//! `discard`, `revert` and `pending_changes` (Task 11); `stranded_sessions`
//! and `adopt_changeset` (Task 12). Each task's tools are one
//! `#[tool_router]` block with its own router, joined in `OkfServer::tools`.

use std::time::Duration;

use axum::http::request::Parts;
use chrono::{DateTime, TimeDelta, Utc};
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
