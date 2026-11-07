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
