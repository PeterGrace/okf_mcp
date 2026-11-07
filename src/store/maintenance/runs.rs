//! Short, catalog-serialized transactions for leased maintenance batches.

use std::collections::HashSet;

use chrono::{DateTime, Duration, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};

use super::{candidates, lease_end, lock, receipt, recover, refused, replay, validate};
use crate::error::OkfError;
use crate::maintenance::select;
use crate::maintenance::types::new_id;
use crate::maintenance::types::{
    Actor, Assessment, ClaimResult, Evidence, Lane, Lease, RevisionPin, RunState, WorkOutcome,
};

/// A deterministic ascending page; the cursor is the last returned row ID.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    /// Rows ordered by creation time, then ID.
    pub items: Vec<T>,
    /// A catalog-scoped cursor, present only when another row exists.
    pub next_after: Option<String>,
}

/// A historical run and its bounded primary work, without lease credentials.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RunReport {
    /// Server-recorded run metadata and outcome counts.
    pub run: RunSummary,
    /// At most 50 primary attempts, ordered by attempt time, then ID.
    pub work: Vec<WorkReport>,
}

/// A primary attempt with its original assessment evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkReport {
    /// Opaque work ID.
    pub id: String,
    /// Original database-form path and immutable revision.
    pub pin: RevisionPin,
    /// Selection lanes recorded at claim time.
    pub reasons: Vec<Lane>,
    /// Terminal outcome, absent while leased.
    pub outcome: Option<WorkOutcome>,
    /// Attributed investigator assessment, never human verification.
    pub assessment: Option<Assessment>,
    /// Submitted summary, absent before submission.
    pub summary: Option<String>,
    /// Original assessment evidence, not the findings' separate evidence.
    pub evidence: Vec<Evidence>,
    /// Finding IDs ordered by creation time, then ID; retrieve via reports.
    pub finding_ids: Vec<String>,
}

pub(super) fn decode<T: serde::de::DeserializeOwned>(text: String) -> Result<T, OkfError> {
    serde_json::from_value(json!(text)).map_err(|_| refused("invalid stored maintenance enum"))
}

pub(super) fn required_id(id: &str) -> Result<(), OkfError> {
    if id.trim().is_empty() {
        return Err(refused("maintenance ID must not be blank"));
    }
    Ok(())
}

pub(super) fn not_found() -> OkfError {
    OkfError::NotFound {
        path: "maintenance/report".into(),
    }
}

pub(super) async fn snapshot(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, OkfError> {
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

pub(super) async fn page_ids(
    conn: &mut sqlx::PgConnection,
    catalog: i64,
    after: Option<&str>,
    limit: u32,
    findings: bool,
) -> Result<Page<String>, OkfError> {
    if !(1..=200).contains(&limit) {
        return Err(refused(
            "maintenance page size must be 1..=200 (default 20)",
        ));
    }
    let cursor: Option<(DateTime<Utc>, String)> = if let Some(id) = after {
        Some(
            sqlx::query_as(if findings {
                "SELECT created_at,id FROM maintenance_findings WHERE catalog_id=$1 AND id=$2"
            } else {
                "SELECT created_at,id FROM maintenance_runs WHERE catalog_id=$1 AND id=$2"
            })
            .bind(catalog)
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?
            .ok_or_else(not_found)?,
        )
    } else {
        None
    };
    let mut items: Vec<String> = sqlx::query_scalar(if findings {
        "SELECT id FROM maintenance_findings WHERE catalog_id=$1 AND ($2::timestamptz IS NULL OR (created_at,id)>($2,$3)) ORDER BY created_at,id LIMIT $4"
    } else {
        "SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND ($2::timestamptz IS NULL OR (created_at,id)>($2,$3)) ORDER BY created_at,id LIMIT $4"
    }).bind(catalog).bind(cursor.as_ref().map(|c| c.0)).bind(cursor.as_ref().map(|c| c.1.as_str()))
        .bind(i64::from(limit) + 1).fetch_all(conn).await?;
    let next_after = if items.len() > limit as usize {
        items.pop();
        items.last().cloned()
    } else {
        None
    };
    Ok(Page { items, next_after })
}

/// Lists runs ascending by `(created_at,id)` within one explicit catalog.
///
/// `limit` must be 1..=200; callers supply 20 when a page size is omitted.
/// `after` is an existing row ID in this catalog, never an offset or timestamp.
/// Results use one read-only snapshot and do not recover or renew leases.
///
/// # Errors
/// `NotFound` for absent/foreign cursors, `Refused` for invalid limits,
/// or a database error. An empty catalog returns an empty page.
pub async fn list_runs(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&str>,
    limit: u32,
) -> Result<Page<RunSummary>, OkfError> {
    let mut tx = snapshot(pool).await?;
    let ids = page_ids(&mut tx, catalog_id, after, limit, false).await?;
    let mut items = Vec::with_capacity(ids.items.len());
    for id in ids.items {
        items.push(summary(&mut tx, catalog_id, &id).await?);
    }
    tx.commit().await?;
    Ok(Page {
        items,
        next_after: ids.next_after,
    })
}

/// Exports a historical run and its bounded work without any lease tokens.
///
/// `run_id` must be nonblank and belong to `catalog_id`. Source edits/deletions
/// do not erase reports. Findings are exported separately via paged retrieval.
/// The entire result is read from one read-only database snapshot.
///
/// # Errors
/// `Refused` for a blank ID, `NotFound` for an absent/foreign run, or a database error.
pub async fn get_run(pool: &PgPool, catalog_id: i64, run_id: &str) -> Result<RunReport, OkfError> {
    required_id(run_id)?;
    let mut tx = snapshot(pool).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM maintenance_runs WHERE catalog_id=$1 AND id=$2)",
    )
    .bind(catalog_id)
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(not_found());
    }
    let run = summary(&mut tx, catalog_id, run_id).await?;
    let rows = sqlx::query("SELECT id,path,revision,lanes,outcome,assessment,summary FROM maintenance_work WHERE catalog_id=$1 AND run_id=$2 ORDER BY attempted_at,id")
        .bind(catalog_id).bind(run_id).fetch_all(&mut *tx).await?;
    let mut work = Vec::with_capacity(rows.len());
    for row in rows {
        let id: String = row.try_get("id")?;
        let evidence = super::reports::load_evidence(&mut tx, catalog_id, &id, None).await?;
        let finding_ids = sqlx::query_scalar("SELECT id FROM maintenance_findings WHERE catalog_id=$1 AND work_id=$2 ORDER BY created_at,id")
            .bind(catalog_id).bind(&id).fetch_all(&mut *tx).await?;
        work.push(WorkReport {
            id,
            pin: RevisionPin {
                path: row.try_get("path")?,
                revision: row.try_get("revision")?,
            },
            reasons: row
                .try_get::<Vec<String>, _>("lanes")?
                .into_iter()
                .map(decode)
                .collect::<Result<_, _>>()?,
            outcome: row
                .try_get::<Option<String>, _>("outcome")?
                .map(decode)
                .transpose()?,
            assessment: row
                .try_get::<Option<String>, _>("assessment")?
                .map(decode)
                .transpose()?,
            summary: row.try_get("summary")?,
            evidence,
            finding_ids,
        });
    }
    tx.commit().await?;
    Ok(RunReport { run, work })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RunSummary {
    pub id: String,
    pub state: RunState,
    pub actor: String,
    pub created_at: String,
    pub deadline: String,
    pub ended_at: Option<String>,
    pub reason: Option<String>,
    pub primary_count: u32,
    pub completed_count: u32,
    pub failed_count: u32,
    pub inconclusive_count: u32,
    pub superseded_count: u32,
}

pub(super) async fn complete_if_terminal(
    conn: &mut sqlx::PgConnection,
    catalog: i64,
    run: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query("UPDATE maintenance_runs SET state='completed',ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND state='running' AND NOT EXISTS (SELECT 1 FROM maintenance_work WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL)")
        .bind(catalog).bind(run).bind(now).execute(conn).await?;
    Ok(())
}

pub(super) async fn expire_submission(
    conn: &mut sqlx::PgConnection,
    catalog: i64,
    run: &str,
    work: &str,
    now: DateTime<Utc>,
    budget_exhausted: bool,
) -> Result<(), OkfError> {
    sqlx::query("UPDATE maintenance_work SET outcome='failed',assessment='not_assessed',reason='lease_expired',ended_at=$4 WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL AND ($5 OR id=$3)")
        .bind(catalog).bind(run).bind(work).bind(now).bind(budget_exhausted).execute(&mut *conn).await?;
    if budget_exhausted {
        sqlx::query("UPDATE maintenance_runs SET state='failed',ended_at=$3,end_reason='budget_exhausted' WHERE catalog_id=$1 AND id=$2")
            .bind(catalog).bind(run).bind(now).execute(conn).await?;
    } else {
        complete_if_terminal(conn, catalog, run, now).await?;
    }
    Ok(())
}

fn state(text: &str) -> RunState {
    match text {
        "running" => RunState::Running,
        "completed" => RunState::Completed,
        "failed" => RunState::Failed,
        "cancelled" => RunState::Cancelled,
        _ => unreachable!("run state constrained by schema"),
    }
}

async fn summary(
    conn: &mut sqlx::PgConnection,
    catalog: i64,
    id: &str,
) -> Result<RunSummary, OkfError> {
    let row = sqlx::query("SELECT actor,state,created_at,deadline,ended_at,end_reason,primary_count FROM maintenance_runs WHERE catalog_id=$1 AND id=$2")
        .bind(catalog).bind(id).fetch_one(&mut *conn).await?;
    let counts = sqlx::query("SELECT count(*) FILTER (WHERE outcome='completed') AS completed, count(*) FILTER (WHERE outcome='failed') AS failed, count(*) FILTER (WHERE outcome='inconclusive') AS inconclusive, count(*) FILTER (WHERE outcome='superseded') AS superseded FROM maintenance_work WHERE catalog_id=$1 AND run_id=$2")
        .bind(catalog).bind(id).fetch_one(conn).await?;
    let date = |at: DateTime<Utc>| at.to_rfc3339();
    Ok(RunSummary {
        id: id.into(),
        state: state(&row.try_get::<String, _>("state")?),
        actor: row.try_get("actor")?,
        created_at: date(row.try_get("created_at")?),
        deadline: date(row.try_get("deadline")?),
        ended_at: row
            .try_get::<Option<DateTime<Utc>>, _>("ended_at")?
            .map(date),
        reason: row.try_get("end_reason")?,
        primary_count: row
            .try_get::<i32, _>("primary_count")?
            .try_into()
            .expect("run count is at most 50"),
        completed_count: counts
            .try_get::<i64, _>("completed")?
            .try_into()
            .expect("run count is at most 50"),
        failed_count: counts
            .try_get::<i64, _>("failed")?
            .try_into()
            .expect("run count is at most 50"),
        inconclusive_count: counts
            .try_get::<i64, _>("inconclusive")?
            .try_into()
            .expect("run count is at most 50"),
        superseded_count: counts
            .try_get::<i64, _>("superseded")?
            .try_into()
            .expect("run count is at most 50"),
    })
}

/// Opens one bounded batch; a retry returns its original leases unchanged.
///
/// # Errors
/// Refuses invalid limits or conflicting idempotency; propagates database failures.
pub async fn claim(
    pool: &PgPool,
    catalog_id: i64,
    actor: &Actor,
    idempotency_key: &str,
    limit: usize,
    review_interval: Duration,
    now: DateTime<Utc>,
) -> Result<ClaimResult, OkfError> {
    validate(actor, idempotency_key)?;
    if !(1..=50).contains(&limit) {
        return Err(refused("maintenance batch size must be 1..=50"));
    }
    if review_interval < Duration::days(1) {
        return Err(refused(
            "maintenance review interval must be at least one day",
        ));
    }
    let payload = json!({"limit": limit, "review_interval_seconds": review_interval.num_seconds()});
    let mut tx = pool.begin().await?;
    lock(&mut tx, catalog_id, now).await?;
    if let Some(previous) = replay(
        &mut tx,
        catalog_id,
        actor,
        "claim",
        idempotency_key,
        &payload,
    )
    .await?
    {
        return Ok(previous);
    }
    recover(&mut tx, catalog_id, now).await?;
    let cursors = candidates::cursors(&mut tx, catalog_id).await?;
    // Load once after locking: deprecated/machinery exclusions are not source changes.
    let locked: Vec<(String, i32)> = sqlx::query_as("SELECT path,revision FROM documents WHERE catalog_id=$1 AND deleted_at IS NULL AND kind='concept' ORDER BY path FOR SHARE")
        .bind(catalog_id).fetch_all(&mut *tx).await?;
    let locked: HashSet<_> = locked.into_iter().collect();
    let mut rows = candidates::load(&mut tx, catalog_id, now).await?;
    // Defer inserts absent from the lock statement snapshot, never lock out of order.
    rows.retain(|candidate| locked.contains(&(candidate.pin.path.clone(), candidate.pin.revision)));
    let selection = select::select(&rows, &cursors, now, review_interval, limit)?;
    let id = new_id()?;
    let deadline = now + Duration::seconds(3_600);
    sqlx::query("INSERT INTO maintenance_runs (id,catalog_id,key_id,credential_id,actor,state,deadline,created_at,ended_at,primary_count) VALUES ($1,$2,(SELECT id FROM api_keys WHERE id=$3),$3,$4,$5,$6,$7,$8,$9)")
        .bind(&id).bind(catalog_id).bind(&actor.key_id).bind(&actor.actor)
        .bind(if selection.items.is_empty() { "completed" } else { "running" })
        .bind(deadline).bind(now).bind(selection.items.is_empty().then_some(now)).bind(selection.items.len() as i32)
        .execute(&mut *tx).await?;
    let mut leases = Vec::new();
    for selected in &selection.items {
        let work_id = new_id()?;
        let token = new_id()?;
        let expiry = lease_end(now, deadline);
        let lanes: Vec<&str> = selected
            .reasons
            .iter()
            .map(|r| match r {
                Lane::Changed => "changed",
                Lane::Aging => "aging",
            })
            .collect();
        let group = selected.aging_group.map(|g| match g {
            crate::maintenance::types::AgingGroup::Expiry => "expiry",
            crate::maintenance::types::AgingGroup::Verification => "verification",
        });
        sqlx::query("INSERT INTO maintenance_work (id,run_id,catalog_id,path,revision,lanes,aging_group,lease_token,lease_expires_at,attempted_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)")
            .bind(&work_id).bind(&id).bind(catalog_id).bind(&selected.pin.path).bind(selected.pin.revision)
            .bind(&lanes).bind(group).bind(&token).bind(expiry).bind(now).execute(&mut *tx).await?;
        leases.push(Lease {
            run_id: id.clone(),
            work_id,
            token,
            pin: selected.pin.clone(),
            reasons: selected.reasons.clone(),
            expires_at: expiry.to_rfc3339(),
        });
    }
    let result = ClaimResult { run_id: id, leases };
    receipt(
        &mut tx,
        catalog_id,
        actor,
        "claim",
        idempotency_key,
        &payload,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

/// Renews an owned, active lease up to the run deadline.
///
/// # Errors
/// Refuses expired, foreign, or terminal leases; propagates database failures.
pub async fn renew(
    pool: &PgPool,
    catalog_id: i64,
    actor: &Actor,
    work_id: &str,
    token: &str,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<Lease, OkfError> {
    validate(actor, idempotency_key)?;
    if work_id.trim().is_empty() || token.trim().is_empty() {
        return Err(refused("work ID and lease token are required"));
    }
    let payload = json!({"work_id": work_id, "token": token});
    let mut tx = pool.begin().await?;
    lock(&mut tx, catalog_id, now).await?;
    // Ownership is checked before replay; old tokens must not cross credential boundaries.
    let row = sqlx::query("SELECT w.run_id,w.path,w.revision,w.lanes,w.lease_token,w.lease_expires_at,w.outcome,r.credential_id,r.state,r.deadline FROM maintenance_work w JOIN maintenance_runs r ON r.catalog_id=w.catalog_id AND r.id=w.run_id WHERE w.catalog_id=$1 AND w.id=$2 FOR UPDATE OF r,w")
        .bind(catalog_id).bind(work_id).fetch_optional(&mut *tx).await?;
    let Some(row) = row else {
        return Err(refused("no such maintenance lease"));
    };
    if row.try_get::<String, _>("credential_id")? != actor.key_id
        || row.try_get::<String, _>("lease_token")? != token
    {
        return Err(refused("no such maintenance lease"));
    }
    if let Some(previous) = replay(
        &mut tx,
        catalog_id,
        actor,
        "renew",
        idempotency_key,
        &payload,
    )
    .await?
    {
        return Ok(previous);
    }
    recover(&mut tx, catalog_id, now).await?;
    let current_state: Option<String> =
        sqlx::query_scalar("SELECT outcome FROM maintenance_work WHERE catalog_id=$1 AND id=$2")
            .bind(catalog_id)
            .bind(work_id)
            .fetch_one(&mut *tx)
            .await?;
    let expiry: DateTime<Utc> = row.try_get("lease_expires_at")?;
    let deadline: DateTime<Utc> = row.try_get("deadline")?;
    if current_state.is_some()
        || row.try_get::<String, _>("state")? != "running"
        || now >= expiry
        || now >= deadline
    {
        tx.commit().await?;
        return Err(refused("maintenance lease has expired or ended"));
    }
    let current: Option<i32> = sqlx::query_scalar(
        "SELECT revision FROM documents WHERE catalog_id=$1 AND path=$2 AND deleted_at IS NULL FOR SHARE",
    )
    .bind(catalog_id)
    .bind(row.try_get::<String, _>("path")?)
    .fetch_optional(&mut *tx)
    .await?;
    if current != Some(row.try_get("revision")?) {
        sqlx::query("UPDATE maintenance_work SET outcome='superseded', reason='source_changed', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND outcome IS NULL")
            .bind(catalog_id).bind(work_id).bind(now).execute(&mut *tx).await?;
        let run_id: String = row.try_get("run_id")?;
        sqlx::query("UPDATE maintenance_runs SET state='completed', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND state='running' AND NOT EXISTS (SELECT 1 FROM maintenance_work WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL)")
            .bind(catalog_id).bind(&run_id).bind(now).execute(&mut *tx).await?;
        tx.commit().await?;
        return Err(refused("maintenance source changed"));
    }
    let renewed = lease_end(now, deadline);
    sqlx::query("UPDATE maintenance_work SET lease_expires_at=$3 WHERE catalog_id=$1 AND id=$2")
        .bind(catalog_id)
        .bind(work_id)
        .bind(renewed)
        .execute(&mut *tx)
        .await?;
    let lanes: Vec<String> = row.try_get("lanes")?;
    let result = Lease {
        run_id: row.try_get("run_id")?,
        work_id: work_id.into(),
        token: token.into(),
        pin: crate::maintenance::types::RevisionPin {
            path: row.try_get("path")?,
            revision: row.try_get("revision")?,
        },
        reasons: lanes
            .iter()
            .map(|v| {
                if v == "changed" {
                    Lane::Changed
                } else {
                    Lane::Aging
                }
            })
            .collect(),
        expires_at: renewed.to_rfc3339(),
    };
    receipt(
        &mut tx,
        catalog_id,
        actor,
        "renew",
        idempotency_key,
        &payload,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

/// Closes a run or acknowledges its automatic completion without changing history.
///
/// A failure close acknowledges any already-terminal run without changing its
/// state, time, reason, or work. Completed acknowledgments remain supported.
/// Ownership is checked before replay; acknowledgments record an idempotent receipt.
///
/// # Errors
/// Refuses foreign runs without operator permission, incomplete completion, or invalid state.
#[allow(clippy::too_many_arguments)]
pub async fn close(
    pool: &PgPool,
    catalog_id: i64,
    actor: &Actor,
    run_id: &str,
    desired: RunState,
    rationale: &str,
    may_close_others: bool,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<RunSummary, OkfError> {
    validate(actor, idempotency_key)?;
    crate::maintenance::validate::text(rationale)?;
    if run_id.trim().is_empty() || matches!(desired, RunState::Running) {
        return Err(refused("run ID and terminal state are required"));
    }
    let payload = json!({"run_id":run_id,"state":desired,"rationale":rationale,"may_close_others":may_close_others});
    let mut tx = pool.begin().await?;
    lock(&mut tx, catalog_id, now).await?;
    let owner: Option<String> = sqlx::query_scalar(
        "SELECT credential_id FROM maintenance_runs WHERE catalog_id=$1 AND id=$2 FOR UPDATE",
    )
    .bind(catalog_id)
    .bind(run_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(owner) = owner else {
        return Err(refused("no such maintenance run"));
    };
    if owner != actor.key_id && !may_close_others {
        return Err(refused("no such maintenance run"));
    }
    if let Some(previous) = replay(
        &mut tx,
        catalog_id,
        actor,
        "close",
        idempotency_key,
        &payload,
    )
    .await?
    {
        return Ok(previous);
    }
    recover(&mut tx, catalog_id, now).await?;
    let row =
        sqlx::query("SELECT state,deadline FROM maintenance_runs WHERE catalog_id=$1 AND id=$2")
            .bind(catalog_id)
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
    let status: String = row.try_get("state")?;
    if status == "running" {
        let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM maintenance_work WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL")
            .bind(catalog_id).bind(run_id).fetch_one(&mut *tx).await?;
        if desired == RunState::Completed && remaining > 0 {
            tx.commit().await?;
            return Err(refused("cannot complete a run with active work"));
        }
        if remaining > 0 {
            sqlx::query("UPDATE maintenance_work SET outcome='failed', assessment='not_assessed', reason='run_closed', ended_at=$3 WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL")
                .bind(catalog_id).bind(run_id).bind(now).execute(&mut *tx).await?;
        }
        let target = match desired {
            RunState::Completed => "completed",
            RunState::Failed => "failed",
            RunState::Cancelled => "cancelled",
            RunState::Running => unreachable!(),
        };
        sqlx::query("UPDATE maintenance_runs SET state=$3, ended_at=$4, end_reason=$5 WHERE catalog_id=$1 AND id=$2")
            .bind(catalog_id).bind(run_id).bind(target).bind(now).bind(rationale).execute(&mut *tx).await?;
    } else if !(desired == RunState::Failed
        || status == "completed" && desired == RunState::Completed)
        && (status != "failed" || now < row.try_get::<DateTime<Utc>, _>("deadline")?)
    {
        tx.commit().await?;
        return Err(refused("maintenance run already ended"));
    }
    let result = summary(&mut tx, catalog_id, run_id).await?;
    receipt(
        &mut tx,
        catalog_id,
        actor,
        "close",
        idempotency_key,
        &payload,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}
