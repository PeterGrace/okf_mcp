//! Report-only investigation leases, evidence reads and submissions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::maintenance::types::{ClaimResult, Lease, Receipt, RevisionPin, RunState, Submission};
use crate::mcp::context::ToolContext;
use crate::store::keys::Access;
use crate::store::maintenance::{evidence, reports, runs};

use super::{actor, lease_path, missing_record, MaintenanceResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceClaimParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
    /// One bounded batch (default 10, minimum 1, maximum 50).
    #[schemars(range(min = 1, max = 50))]
    pub batch_size: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceRenewParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque work ID returned by maintenance_claim.
    pub work_id: String,
    /// Current opaque lease token; must belong to the calling credential.
    pub token: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceReadRevisionParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Published concept path in this catalog; never an unpublished changeset path.
    pub path: String,
    /// Positive immutable revision number returned by a claim or history read.
    #[schemars(range(min = 1))]
    pub revision: i32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceSubmitParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque work ID returned by maintenance_claim.
    pub work_id: String,
    /// Current opaque lease token; must belong to the calling credential.
    pub token: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
    /// Revision-pinned assessment and proposed findings; never applies canonical edits.
    pub submission: Submission,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceCloseParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque run ID belonging to the named catalog.
    pub run_id: String,
    /// Requested terminal state; completion requires all work to have ended.
    #[schemars(schema_with = "close_state_schema")]
    pub state: RunState,
    /// Human-readable reason recorded with attribution; secrets and oversized text are refused.
    pub rationale: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
}

fn close_state_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "enum": ["completed", "failed", "cancelled"]})
}

pub(crate) async fn maintenance_claim(
    ctx: &ToolContext,
    params: MaintenanceClaimParams,
) -> Result<MaintenanceResult<ClaimResult>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require_maintenance_report(state.name())?;
    let mut result = runs::claim(
        ctx.app().pool(),
        state.id(),
        &actor(ctx),
        &params.idempotency_key,
        params.batch_size.unwrap_or(10) as usize,
        ctx.app().config().maintenance_review_interval(state.name()),
        ctx.app().clock().now(),
    )
    .await?;
    for lease in &mut result.leases {
        lease_path(lease);
    }
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_renew(
    ctx: &ToolContext,
    params: MaintenanceRenewParams,
) -> Result<MaintenanceResult<Lease>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require_maintenance_report(state.name())?;
    let mut result = runs::renew(
        ctx.app().pool(),
        state.id(),
        &actor(ctx),
        &params.work_id,
        &params.token,
        &params.idempotency_key,
        ctx.app().clock().now(),
    )
    .await
    .map_err(missing_record)?;
    lease_path(&mut result);
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_read_revision(
    ctx: &ToolContext,
    params: MaintenanceReadRevisionParams,
) -> Result<MaintenanceResult<evidence::PinnedConcept>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let pin = RevisionPin {
        path: params.path,
        revision: params.revision,
    };
    // Normalize once at the boundary; the store's exact-byte reader takes a stored path.
    let pin = crate::maintenance::validate::pin(&pin)?;
    let mut result = evidence::read(ctx.app().pool(), state.id(), &pin).await?;
    super::pin_path(&mut result.pin);
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_submit(
    ctx: &ToolContext,
    params: MaintenanceSubmitParams,
) -> Result<MaintenanceResult<Receipt>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require_maintenance_report(state.name())?;
    let result = reports::submit(
        ctx.app().pool(),
        state.id(),
        &actor(ctx),
        &params.work_id,
        &params.token,
        &params.idempotency_key,
        &params.submission,
        ctx.app().clock().now(),
    )
    .await?;
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_close(
    ctx: &ToolContext,
    params: MaintenanceCloseParams,
) -> Result<MaintenanceResult<runs::RunSummary>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    // Ordinary Write is an operator override, never a client-supplied option.
    let operator = ctx.principal().require(Access::Write, state.name()).is_ok();
    if !operator {
        ctx.principal().require_maintenance_report(state.name())?;
    }
    if params.run_id.trim().is_empty() {
        return Err(OkfError::Refused {
            reason: "maintenance ID must not be blank".into(),
        });
    }
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT credential_id FROM maintenance_runs WHERE catalog_id=$1 AND id=$2",
    )
    .bind(state.id())
    .bind(&params.run_id)
    .fetch_optional(ctx.app().pool())
    .await?;
    let owner = owner.ok_or_else(super::not_found)?;
    let own = owner == ctx.principal().key_id;
    if own {
        ctx.principal().require_maintenance_report(state.name())?;
    } else if !operator {
        return Err(super::not_found());
    }
    // Own-run retries keep the same store payload even if ordinary access changes.
    let result = runs::close(
        ctx.app().pool(),
        state.id(),
        &actor(ctx),
        &params.run_id,
        params.state,
        &params.rationale,
        !own && operator,
        &params.idempotency_key,
        ctx.app().clock().now(),
    )
    .await
    .map_err(missing_record)?;
    Ok(MaintenanceResult::new(state.name(), result))
}

/// M13 Task 3: every maintenance work tool checks access through
/// `Principal::require` before it reads its ids, so a suspended catalog is
/// refused to a non-admin reporter with `Suspended`, stays unknown to a
/// reporter that does not reach it, and still answers an admin.
#[cfg(test)]
mod suspension_tests {
    use super::*;

    use crate::config::Config;
    use crate::maintenance::types::{Assessment, WorkOutcome};
    use crate::mcp::testing::{
        app_with, cached_principal, context, stored_secret, suspend_catalogs, TestApp,
    };

    /// Each work tool on `catalog` as the key behind `secret`, as `(tool,
    /// error kind or "ok")`. The ids are made up: access is checked first.
    async fn every_tool(
        test: &TestApp,
        secret: &str,
        catalog: &str,
    ) -> Vec<(&'static str, String)> {
        fn outcome<T>(result: Result<T, OkfError>) -> String {
            result.map_or_else(|error| error.kind().to_string(), |_| "ok".to_string())
        }
        let ctx = context(test, cached_principal(test, secret), None);
        let catalog = catalog.to_string();
        let submission = Submission {
            outcome: WorkOutcome::Completed,
            assessment: Assessment::NotAssessed,
            summary: "Nothing to report.".to_string(),
            evidence: Vec::new(),
            findings: Vec::new(),
            reported_tokens: None,
        };
        vec![
            (
                "maintenance_claim",
                outcome(
                    maintenance_claim(
                        &ctx,
                        MaintenanceClaimParams {
                            catalog: catalog.clone(),
                            idempotency_key: "claim".to_string(),
                            batch_size: Some(1),
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_renew",
                outcome(
                    maintenance_renew(
                        &ctx,
                        MaintenanceRenewParams {
                            catalog: catalog.clone(),
                            work_id: "no-such-work".to_string(),
                            token: "no-such-token".to_string(),
                            idempotency_key: "renew".to_string(),
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_read_revision",
                outcome(
                    maintenance_read_revision(
                        &ctx,
                        MaintenanceReadRevisionParams {
                            catalog: catalog.clone(),
                            path: "a.md".to_string(),
                            revision: 1,
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_submit",
                outcome(
                    maintenance_submit(
                        &ctx,
                        MaintenanceSubmitParams {
                            catalog: catalog.clone(),
                            work_id: "no-such-work".to_string(),
                            token: "no-such-token".to_string(),
                            idempotency_key: "submit".to_string(),
                            submission,
                        },
                    )
                    .await,
                ),
            ),
            (
                "maintenance_close",
                outcome(
                    maintenance_close(
                        &ctx,
                        MaintenanceCloseParams {
                            catalog,
                            run_id: "no-such-run".to_string(),
                            state: RunState::Cancelled,
                            rationale: "Stop.".to_string(),
                            idempotency_key: "close".to_string(),
                        },
                    )
                    .await,
                ),
            ),
        ]
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_work_tool_refuses_a_suspended_catalog_but_to_an_admin() {
        let note = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n";
        let test = app_with(
            &[("pg", &[("a.md", note)]), ("other", &[])],
            Config::default(),
        )
        .await;
        let reporter = stored_secret(&test, "reporter", Access::Write, &["pg"], true).await;
        let stranger = stored_secret(&test, "stranger", Access::Write, &["other"], true).await;
        let root = stored_secret(&test, "root", Access::Admin, &[], true).await;
        suspend_catalogs(&test, &["pg"]).await;

        let suspended = every_tool(&test, &reporter, "pg").await;
        let strangers = every_tool(&test, &stranger, "pg").await;
        let admins = every_tool(&test, &root, "pg").await;

        for (tool, kind) in &suspended {
            assert_eq!(kind, "Suspended", "{tool} as the catalog's reporter");
        }
        for (tool, kind) in &strangers {
            assert_eq!(
                kind, "CatalogNotSelected",
                "{tool}: an unreached catalog never reads as suspended"
            );
        }
        for (tool, kind) in &admins {
            assert_ne!(kind, "Suspended", "{tool} as an admin");
        }
        assert_eq!(admins[0].1, "ok", "an admin still claims");
        assert_eq!(admins[2].1, "ok", "an admin still reads a revision");
    }
}
