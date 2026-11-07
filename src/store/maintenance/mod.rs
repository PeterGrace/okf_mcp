//! Transaction helpers shared by maintenance store mutations.

pub mod candidates;
pub mod evidence;
pub mod reports;
pub mod runs;

#[cfg(test)]
mod report_tests;
#[cfg(test)]
mod submission_tests;

use chrono::{DateTime, Duration, Utc};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sqlx::{PgConnection, Row};

use crate::error::OkfError;
use crate::maintenance::types::Actor;
use crate::okf::analysis::secrets::scan_secrets;
use crate::store::changesets;

fn refused(reason: &str) -> OkfError {
    OkfError::Refused {
        reason: reason.into(),
    }
}

fn validate(actor: &Actor, key: &str) -> Result<(), OkfError> {
    if actor.key_id.trim().is_empty() || actor.actor.trim().is_empty() {
        return Err(refused(
            "maintenance actor and credential ID must not be blank",
        ));
    }
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii() && !b.is_ascii_whitespace() && !b.is_ascii_control())
    {
        return Err(refused(
            "idempotency key must be 1..=128 non-whitespace ASCII characters",
        ));
    }
    if let Some(hit) = scan_secrets("maintenance/idempotency_key", key).first() {
        return Err(OkfError::SecretDetected {
            path: "maintenance/idempotency_key".into(),
            line: hit.line,
            rule: hit.rule.clone(),
            message: hit.message.clone(),
        });
    }
    Ok(())
}

async fn lock(
    conn: &mut PgConnection,
    catalog_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // Match publish/delete order: advisory lock, catalog identity, maintenance state,
    // run/work rows, then path-ordered source rows. Imports write before deleting.
    crate::store::catalogs::lock(conn, catalog_id).await?;
    changesets::lock_catalog(conn, catalog_id).await?;
    sqlx::query("INSERT INTO maintenance_state (catalog_id, updated_at) VALUES ($1,$2) ON CONFLICT (catalog_id) DO NOTHING")
        .bind(catalog_id).bind(now).execute(&mut *conn).await?;
    sqlx::query("SELECT catalog_id FROM maintenance_state WHERE catalog_id=$1 FOR UPDATE")
        .bind(catalog_id)
        .fetch_one(conn)
        .await?;
    Ok(())
}

async fn replay<T: DeserializeOwned>(
    conn: &mut PgConnection,
    catalog: i64,
    actor: &Actor,
    operation: &str,
    key: &str,
    payload: &Value,
) -> Result<Option<T>, OkfError> {
    let row = sqlx::query("SELECT request, response FROM maintenance_requests WHERE catalog_id=$1 AND credential_id=$2 AND operation=$3 AND idempotency_key=$4")
        .bind(catalog).bind(&actor.key_id).bind(operation).bind(key).fetch_optional(conn).await?;
    let Some(row) = row else {
        return Ok(None);
    };
    let request: Value = row.try_get("request")?;
    if request != *payload {
        return Err(refused("idempotency key reused with a different request"));
    }
    let response: Value = row.try_get("response")?;
    Ok(Some(serde_json::from_value(response).map_err(|_| {
        refused("stored maintenance response is invalid")
    })?))
}

async fn receipt<T: Serialize>(
    conn: &mut PgConnection,
    catalog: i64,
    actor: &Actor,
    operation: &str,
    key: &str,
    payload: &Value,
    response: &T,
) -> Result<(), OkfError> {
    sqlx::query("INSERT INTO maintenance_requests (catalog_id, credential_id, operation, idempotency_key, request, response) VALUES ($1,$2,$3,$4,$5,$6)")
        .bind(catalog).bind(&actor.key_id).bind(operation).bind(key).bind(payload)
        .bind(serde_json::to_value(response).map_err(|_| refused("maintenance response cannot be serialized"))?)
        .execute(conn).await?;
    Ok(())
}

async fn recover(
    conn: &mut PgConnection,
    catalog: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // Runs are locked before work, and exhausted deadlines win over expired leases.
    let expired: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND state='running' AND deadline <= $2 ORDER BY created_at,id FOR UPDATE")
        .bind(catalog).bind(now).fetch_all(&mut *conn).await?;
    for run in expired {
        sqlx::query("UPDATE maintenance_work SET outcome='failed', assessment='not_assessed', reason='lease_expired', ended_at=$3 WHERE catalog_id=$1 AND run_id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&run).bind(now).execute(&mut *conn).await?;
        sqlx::query("UPDATE maintenance_runs SET state='failed', ended_at=$3, end_reason='budget_exhausted' WHERE catalog_id=$1 AND id=$2")
            .bind(catalog).bind(&run).bind(now).execute(&mut *conn).await?;
    }
    let _active_runs: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_runs WHERE catalog_id=$1 AND state='running' ORDER BY created_at,id FOR UPDATE")
        .bind(catalog).fetch_all(&mut *conn).await?;
    let expiring: Vec<String> = sqlx::query_scalar("SELECT id FROM maintenance_work WHERE catalog_id=$1 AND outcome IS NULL AND lease_expires_at <= $2 ORDER BY run_id,id FOR UPDATE")
        .bind(catalog).bind(now).fetch_all(&mut *conn).await?;
    for id in expiring {
        sqlx::query("UPDATE maintenance_work SET outcome='failed', assessment='not_assessed', reason='lease_expired', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&id).bind(now).execute(&mut *conn).await?;
    }
    // If a source changed, end only the old revision attempt.
    let stale: Vec<String> = sqlx::query_scalar("SELECT w.id FROM maintenance_work w LEFT JOIN documents d ON d.catalog_id=w.catalog_id AND d.path=w.path WHERE w.catalog_id=$1 AND w.outcome IS NULL AND (d.revision IS DISTINCT FROM w.revision OR d.deleted_at IS NOT NULL) ORDER BY w.run_id,w.id FOR UPDATE OF w")
        .bind(catalog).fetch_all(&mut *conn).await?;
    for id in stale {
        sqlx::query("UPDATE maintenance_work SET outcome='superseded', reason='source_changed', ended_at=$3 WHERE catalog_id=$1 AND id=$2 AND outcome IS NULL")
            .bind(catalog).bind(&id).bind(now).execute(&mut *conn).await?;
    }
    sqlx::query("UPDATE maintenance_runs r SET state='completed', ended_at=$2 WHERE r.catalog_id=$1 AND r.state='running' AND NOT EXISTS (SELECT 1 FROM maintenance_work w WHERE w.catalog_id=$1 AND w.run_id=r.id AND w.outcome IS NULL)")
        .bind(catalog).bind(now).execute(conn).await?;
    Ok(())
}

fn lease_end(now: DateTime<Utc>, deadline: DateTime<Utc>) -> DateTime<Utc> {
    (now + Duration::seconds(900)).min(deadline)
}

#[cfg(test)]
mod final_tests;
#[cfg(test)]
mod tests;
