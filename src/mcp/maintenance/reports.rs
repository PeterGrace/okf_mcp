//! Read-only maintenance reports and ordinary-Write finding dispositions.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::maintenance::types::Disposition;
use crate::mcp::context::ToolContext;
use crate::store::keys::Access;
use crate::store::maintenance::{reports, runs};

use super::{actor, evidence_paths, finding_paths, pin_path, MaintenanceResult};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceListRunsParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Exclusive last-row ID from the previous page in this catalog; omit for the first page.
    pub after: Option<String>,
    /// Page size (default 20, minimum 1, maximum 200).
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceGetRunParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque run ID belonging to the named catalog.
    pub run_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceListFindingsParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Exclusive last-row ID from the previous page in this catalog; omit for the first page.
    pub after: Option<String>,
    /// Page size (default 20, minimum 1, maximum 200).
    #[schemars(range(min = 1, max = 200))]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceGetFindingParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque finding ID belonging to the named catalog.
    pub finding_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MaintenanceDisposeParams {
    /// Required authorized catalog name; connection defaults do not apply.
    pub catalog: String,
    /// Opaque finding ID belonging to the named catalog.
    pub finding_id: String,
    /// Human follow-up decision; acceptance does not apply the proposed change.
    pub disposition: Disposition,
    /// Human-readable reason recorded with attribution; secrets and oversized text are refused.
    pub rationale: String,
    /// Caller-chosen retry key (1..=128 non-whitespace ASCII bytes); reuse only for identical input.
    pub idempotency_key: String,
}

pub(crate) async fn maintenance_list_runs(
    ctx: &ToolContext,
    params: MaintenanceListRunsParams,
) -> Result<MaintenanceResult<runs::Page<runs::RunSummary>>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let result = runs::list_runs(
        ctx.app().pool(),
        state.id(),
        params.after.as_deref(),
        params.limit.unwrap_or(20),
    )
    .await?;
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_get_run(
    ctx: &ToolContext,
    params: MaintenanceGetRunParams,
) -> Result<MaintenanceResult<runs::RunReport>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let mut result = runs::get_run(ctx.app().pool(), state.id(), &params.run_id).await?;
    for work in &mut result.work {
        pin_path(&mut work.pin);
        evidence_paths(&mut work.evidence);
    }
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_list_findings(
    ctx: &ToolContext,
    params: MaintenanceListFindingsParams,
) -> Result<MaintenanceResult<runs::Page<reports::FindingRecord>>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let mut result = reports::list_findings(
        ctx.app().pool(),
        state.id(),
        params.after.as_deref(),
        params.limit.unwrap_or(20),
    )
    .await?;
    for finding in &mut result.items {
        finding_paths(finding);
    }
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_get_finding(
    ctx: &ToolContext,
    params: MaintenanceGetFindingParams,
) -> Result<MaintenanceResult<reports::FindingRecord>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Read, state.name())?;
    let mut result = reports::get_finding(ctx.app().pool(), state.id(), &params.finding_id).await?;
    finding_paths(&mut result);
    Ok(MaintenanceResult::new(state.name(), result))
}

pub(crate) async fn maintenance_dispose(
    ctx: &ToolContext,
    params: MaintenanceDisposeParams,
) -> Result<MaintenanceResult<reports::FindingRecord>, OkfError> {
    let state = ctx.one(Some(&params.catalog))?;
    ctx.principal().require(Access::Write, state.name())?;
    let mut result = reports::dispose(
        ctx.app().pool(),
        state.id(),
        &actor(ctx),
        &params.finding_id,
        params.disposition,
        &params.rationale,
        &params.idempotency_key,
        ctx.app().clock().now(),
    )
    .await?;
    finding_paths(&mut result);
    Ok(MaintenanceResult::new(state.name(), result))
}
