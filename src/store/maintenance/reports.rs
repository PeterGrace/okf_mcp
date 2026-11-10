//! Revision-pinned reports and append-only human decisions, never canonical edits.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, PgPool, Row};

use super::{evidence, lock, receipt, refused, replay, runs, validate};
use crate::error::OkfError;
use crate::maintenance::types::{
    new_id, Actor, Disposition, Evidence, FindingInput, Lane, Receipt, RevisionPin, Submission,
    WorkOutcome,
};
use crate::maintenance::validate as report;

/// An attributed finding, its append-only human history, and read-time freshness.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindingRecord {
    /// Opaque finding ID.
    pub id: String,
    /// Run that produced this finding.
    pub run_id: String,
    /// Primary investigation attempt that produced this finding.
    pub work_id: String,
    /// Original investigator input with immutable local evidence.
    pub input: FindingInput,
    /// Investigator actor snapshot from the originating run.
    pub actor: String,
    /// Server-recorded submission time in UTC RFC 3339 form.
    pub created_at: String,
    /// Last appended disposition, or Open when history is empty.
    pub disposition: Disposition,
    /// All human dispositions in append order (monotonic ID, not wall-clock order).
    pub dispositions: Vec<DispositionRecord>,
    /// True if the primary or any evidence pin in its work is no longer live/current.
    pub outdated: bool,
}

/// One immutable human decision; acceptance never applies canonical edits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DispositionRecord {
    /// Server-assigned append-order ID.
    pub id: i64,
    /// Human decision, including Open to reopen a finding.
    pub disposition: Disposition,
    /// Original bounded, secret-scanned human rationale.
    pub rationale: String,
    /// Authenticated actor snapshot supplied by the server, not request text.
    pub actor: String,
    /// Server clock timestamp in UTC RFC 3339 form.
    pub at: String,
}

pub(super) async fn load_evidence(
    conn: &mut PgConnection,
    catalog: i64,
    work: &str,
    finding: Option<&str>,
) -> Result<Vec<Evidence>, OkfError> {
    let rows = sqlx::query("SELECT path,revision,excerpt FROM maintenance_evidence WHERE catalog_id=$1 AND work_id=$2 AND finding_id IS NOT DISTINCT FROM $3 ORDER BY ordinal")
        .bind(catalog).bind(work).bind(finding).fetch_all(conn).await?;
    rows.into_iter()
        .map(|row| {
            Ok(Evidence {
                pin: RevisionPin {
                    path: row.try_get("path")?,
                    revision: row.try_get("revision")?,
                },
                excerpt: row.try_get("excerpt")?,
            })
        })
        .collect()
}

async fn finding(
    conn: &mut PgConnection,
    catalog: i64,
    id: &str,
) -> Result<FindingRecord, OkfError> {
    let row = sqlx::query("SELECT f.run_id,f.work_id,f.category,f.assertion,f.follow_up,f.uncertainty,f.relationship,f.external,f.created_at,r.actor, \
        EXISTS (SELECT 1 FROM maintenance_work w LEFT JOIN documents d ON d.catalog_id=w.catalog_id AND d.path=w.path WHERE w.catalog_id=f.catalog_id AND w.id=f.work_id AND (d.revision IS DISTINCT FROM w.revision OR d.deleted_at IS NOT NULL OR d.kind IS DISTINCT FROM 'concept')) \
        OR EXISTS (SELECT 1 FROM maintenance_evidence e LEFT JOIN documents d ON d.catalog_id=e.catalog_id AND d.path=e.path WHERE e.catalog_id=f.catalog_id AND e.work_id=f.work_id AND (d.revision IS DISTINCT FROM e.revision OR d.deleted_at IS NOT NULL OR d.kind IS DISTINCT FROM 'concept')) AS outdated \
        FROM maintenance_findings f JOIN maintenance_runs r ON r.catalog_id=f.catalog_id AND r.id=f.run_id WHERE f.catalog_id=$1 AND f.id=$2")
        .bind(catalog).bind(id).fetch_optional(&mut *conn).await?.ok_or_else(runs::not_found)?;
    let work_id: String = row.try_get("work_id")?;
    let evidence = load_evidence(conn, catalog, &work_id, Some(id)).await?;
    let rows = sqlx::query("SELECT id,disposition,rationale,actor,created_at FROM maintenance_dispositions WHERE catalog_id=$1 AND finding_id=$2 ORDER BY id")
        .bind(catalog).bind(id).fetch_all(conn).await?;
    let dispositions = rows
        .into_iter()
        .map(|row| {
            Ok(DispositionRecord {
                id: row.try_get("id")?,
                disposition: runs::decode(row.try_get("disposition")?)?,
                rationale: row.try_get("rationale")?,
                actor: row.try_get("actor")?,
                at: row.try_get::<DateTime<Utc>, _>("created_at")?.to_rfc3339(),
            })
        })
        .collect::<Result<Vec<_>, OkfError>>()?;
    Ok(FindingRecord {
        id: id.into(),
        run_id: row.try_get("run_id")?,
        work_id,
        input: FindingInput {
            category: runs::decode(row.try_get("category")?)?,
            assertion: row.try_get("assertion")?,
            follow_up: row.try_get("follow_up")?,
            uncertainty: row.try_get("uncertainty")?,
            relationship: runs::decode(row.try_get("relationship")?)?,
            evidence,
            external: serde_json::from_value(row.try_get("external")?)
                .map_err(|_| refused("invalid stored maintenance observations"))?,
        },
        actor: row.try_get("actor")?,
        created_at: row.try_get::<DateTime<Utc>, _>("created_at")?.to_rfc3339(),
        disposition: dispositions
            .last()
            .map_or(Disposition::Open, |d| d.disposition),
        dispositions,
        outdated: row.try_get("outdated")?,
    })
}

/// Retrieves an immutable finding with human history and current pin freshness.
///
/// All evidence pins in the originating work, including its primary, are checked
/// against live concept rows in one read-only snapshot. Deleted/recreated paths
/// do not make old revisions current. No token or credential ID is exported.
///
/// # Errors
/// `Refused` for blank IDs, `NotFound` for absent/foreign findings, or database errors.
pub async fn get_finding(
    pool: &PgPool,
    catalog_id: i64,
    finding_id: &str,
) -> Result<FindingRecord, OkfError> {
    runs::required_id(finding_id)?;
    let mut tx = runs::snapshot(pool).await?;
    let result = finding(&mut tx, catalog_id, finding_id).await?;
    tx.commit().await?;
    Ok(result)
}

/// Lists findings ascending by `(created_at,id)` in one read-only snapshot.
///
/// `limit` must be 1..=200; callers supply the default 20 when omitted upstream.
/// `after` is an existing finding ID in this explicit catalog. Freshness is
/// computed at read time, not copied from the original submission receipt.
///
/// # Errors
/// `Refused` for invalid limits, `NotFound` for absent/foreign cursors, or database errors.
pub async fn list_findings(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&str>,
    limit: u32,
) -> Result<runs::Page<FindingRecord>, OkfError> {
    list_findings_with(pool, catalog_id, after, limit, None).await
}

/// [`list_findings`], keeping only findings whose current disposition (the
/// last appended, else `Open`) is `disposition` when it is set. `limit` and
/// `next_after` count matching findings only; `after` may be any finding of
/// the catalog, so a cursor survives its finding changing disposition.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
/// * `after` - the previous page's last finding ID, if any.
/// * `limit` - page size, 1..=200.
/// * `disposition` - keep only this current disposition; `None` for all.
///
/// # Returns
/// One page of findings and the cursor for the next, if another matches.
///
/// # Errors
/// As [`list_findings`].
pub async fn list_findings_with(
    pool: &PgPool,
    catalog_id: i64,
    after: Option<&str>,
    limit: u32,
    disposition: Option<Disposition>,
) -> Result<runs::Page<FindingRecord>, OkfError> {
    let mut tx = runs::snapshot(pool).await?;
    let ids = runs::page_ids(
        &mut tx,
        catalog_id,
        after,
        limit,
        runs::Listing::Findings(disposition),
    )
    .await?;
    let mut items = Vec::with_capacity(ids.items.len());
    for id in ids.items {
        items.push(finding(&mut tx, catalog_id, &id).await?);
    }
    tx.commit().await?;
    Ok(runs::Page {
        items,
        next_after: ids.next_after,
    })
}

/// How many of a catalog's findings are in each current disposition.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FindingCounts {
    /// Open, including reopened.
    pub open: i64,
    /// Accepted for follow-up.
    pub accepted_for_follow_up: i64,
    /// Dismissed.
    pub dismissed: i64,
    /// Waiting for input.
    pub needs_input: i64,
}

/// Counts a catalog's findings by current disposition, in one query.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the catalog.
///
/// # Returns
/// The counts; every disposition without findings is 0.
///
/// # Errors
/// Database errors; `Refused` for a stored disposition outside the four values.
pub async fn count_findings(pool: &PgPool, catalog_id: i64) -> Result<FindingCounts, OkfError> {
    // The inner select computes each finding's current disposition, the outer
    // one groups them (grouping by a correlated subquery directly is refused).
    let rows: Vec<(String, i64)> = sqlx::query_as(concat!(
        "SELECT disposition, COUNT(*) FROM (SELECT ",
        current_disposition!(),
        " AS disposition FROM maintenance_findings f WHERE f.catalog_id=$1) current \
         GROUP BY disposition"
    ))
    .bind(catalog_id)
    .fetch_all(pool)
    .await?;
    let mut counts = FindingCounts::default();
    for (text, n) in rows {
        // `&mut counts.open` borrows one field for writing; `*slot = n` writes through it.
        let slot = match runs::decode::<Disposition>(text)? {
            Disposition::Open => &mut counts.open,
            Disposition::AcceptedForFollowUp => &mut counts.accepted_for_follow_up,
            Disposition::Dismissed => &mut counts.dismissed,
            Disposition::NeedsInput => &mut counts.needs_input,
        };
        *slot = n;
    }
    Ok(counts)
}

/// Appends a human decision without applying it to canonical content.
///
/// The caller must authenticate and authorize ordinary Write for `catalog_id`
/// before invoking this store API, including on retries. `actor` and `now` are
/// supplied by that authenticated server context/clock. Decisions need not be
/// made by the originating reporter. Reopening appends Open; it erases nothing.
/// Identical credential-scoped retries return the exact original response,
/// including its original history and freshness, without additional writes.
///
/// # Errors
/// `Refused` for invalid IDs/keys, rationale over 4096 UTF-8 bytes or conflicting
/// retries; masked `SecretDetected` for secrets; `NotFound` for absent/foreign
/// findings/catalogs; database errors. All mutation and receipt writes are atomic.
#[allow(clippy::too_many_arguments)]
pub async fn dispose(
    pool: &PgPool,
    catalog_id: i64,
    actor: &Actor,
    finding_id: &str,
    disposition: Disposition,
    rationale: &str,
    idempotency_key: &str,
    now: DateTime<Utc>,
) -> Result<FindingRecord, OkfError> {
    validate(actor, idempotency_key)?;
    runs::required_id(finding_id)?;
    report::text(rationale)?;
    let payload =
        json!({"finding_id": finding_id, "disposition": disposition, "rationale": rationale});
    let mut tx = pool.begin().await?;
    lock(&mut tx, catalog_id, now).await?;
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM maintenance_findings WHERE catalog_id=$1 AND id=$2)",
    )
    .bind(catalog_id)
    .bind(finding_id)
    .fetch_one(&mut *tx)
    .await?;
    if !exists {
        return Err(runs::not_found());
    }
    if let Some(previous) = replay(
        &mut tx,
        catalog_id,
        actor,
        "dispose",
        idempotency_key,
        &payload,
    )
    .await?
    {
        return Ok(previous);
    }
    sqlx::query("INSERT INTO maintenance_dispositions (catalog_id,finding_id,disposition,rationale,credential_id,actor,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7)")
        .bind(catalog_id).bind(finding_id).bind(enum_text(disposition)?).bind(rationale)
        .bind(&actor.key_id).bind(&actor.actor).bind(now).execute(&mut *tx).await?;
    let result = finding(&mut tx, catalog_id, finding_id).await?;
    receipt(
        &mut tx,
        catalog_id,
        actor,
        "dispose",
        idempotency_key,
        &payload,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}

fn missing() -> OkfError {
    OkfError::NotFound {
        path: "maintenance/work".into(),
    }
}

fn stale() -> OkfError {
    OkfError::Busy {
        reason: "maintenance lease has expired or ended".into(),
    }
}

/// The `snake_case` text of a maintenance enum, as stored.
///
/// # Errors
/// `Refused` when `value` does not serialize to a string.
pub(super) fn enum_text(value: impl Serialize) -> Result<String, OkfError> {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .ok_or_else(|| refused("invalid maintenance enum"))
}

async fn store_evidence(
    conn: &mut PgConnection,
    catalog: i64,
    work: &str,
    finding: Option<&str>,
    items: &[Evidence],
) -> Result<(), OkfError> {
    for (ordinal, item) in items.iter().enumerate() {
        sqlx::query("INSERT INTO maintenance_evidence (catalog_id,work_id,finding_id,ordinal,path,revision,excerpt) VALUES ($1,$2,$3,$4,$5,$6,$7)")
            .bind(catalog).bind(work).bind(finding).bind(ordinal as i32).bind(&item.pin.path).bind(item.pin.revision).bind(&item.excerpt)
            .execute(&mut *conn).await?;
    }
    Ok(())
}

/// Stores an owned investigation and its receipt in one transaction.
///
/// # Arguments
/// `catalog_id` is explicit; `actor.key_id` is the immutable authenticated
/// credential ID. `now` comes from the caller's server clock. The lease token
/// and idempotency key identify one attempt and one normalized submission.
///
/// # Returns
/// The original receipt on an identical authorized retry, including after expiry.
/// A still-leased historical primary produces a superseded, outdated receipt.
///
/// # Errors
/// Returns `NotFound` for inaccessible work, `Busy` for stale leases, `Refused`
/// for invalid reports or conflicting retries, and propagates masked secret
/// detection and database errors. Validation failures persist no report writes.
#[allow(clippy::too_many_arguments)]
pub async fn submit(
    pool: &PgPool,
    catalog_id: i64,
    actor: &Actor,
    work_id: &str,
    token: &str,
    idempotency_key: &str,
    input: &Submission,
    now: DateTime<Utc>,
) -> Result<Receipt, OkfError> {
    validate(actor, idempotency_key)?;
    if work_id.trim().is_empty() || token.trim().is_empty() {
        return Err(refused("work ID and lease token are required"));
    }
    report::submission(input)?;
    if input.outcome == WorkOutcome::Superseded {
        return Err(refused("superseded is a server-assigned outcome"));
    }
    let tokens = input
        .reported_tokens
        .map(i64::try_from)
        .transpose()
        .map_err(|_| refused("reported token count exceeds storage limit"))?;
    let mut input = input.clone();
    for item in input.evidence.iter_mut().chain(
        input
            .findings
            .iter_mut()
            .flat_map(|f| f.evidence.iter_mut()),
    ) {
        item.pin = report::pin(&item.pin)?;
    }
    let payload = json!({"work_id":work_id,"token":token,"submission":input});
    let mut tx = pool.begin().await?;
    // The catalog lock protects identity; state serializes maintenance writes.
    // Source row locks below exclude concurrent publication of the cited pins.
    lock(&mut tx, catalog_id, now).await?;
    let run_id: String =
        sqlx::query_scalar("SELECT run_id FROM maintenance_work WHERE catalog_id=$1 AND id=$2")
            .bind(catalog_id)
            .bind(work_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(missing)?;
    let run = sqlx::query("SELECT credential_id,state,deadline FROM maintenance_runs WHERE catalog_id=$1 AND id=$2 FOR UPDATE")
        .bind(catalog_id).bind(&run_id).fetch_one(&mut *tx).await?;
    let work = sqlx::query("SELECT path,revision,lanes,aging_group,lease_token,lease_expires_at,outcome FROM maintenance_work WHERE catalog_id=$1 AND id=$2 FOR UPDATE")
        .bind(catalog_id).bind(work_id).fetch_one(&mut *tx).await?;
    if run.try_get::<String, _>("credential_id")? != actor.key_id
        || work.try_get::<String, _>("lease_token")? != token
    {
        return Err(missing());
    }
    if let Some(previous) = replay(
        &mut tx,
        catalog_id,
        actor,
        "submit",
        idempotency_key,
        &payload,
    )
    .await?
    {
        return Ok(previous);
    }
    if run.try_get::<String, _>("state")? != "running"
        || work.try_get::<Option<String>, _>("outcome")?.is_some()
    {
        return Err(stale());
    }
    let deadline: DateTime<Utc> = run.try_get("deadline")?;
    let expiry: DateTime<Utc> = work.try_get("lease_expires_at")?;
    if now >= deadline || now >= expiry {
        runs::expire_submission(&mut tx, catalog_id, &run_id, work_id, now, now >= deadline)
            .await?;
        tx.commit().await?;
        return Err(stale());
    }
    // Do not call generic recovery here: a changed primary with a valid lease
    // must remain available for this historical report until another claim wins.
    let primary = RevisionPin {
        path: work.try_get("path")?,
        revision: work.try_get("revision")?,
    };
    let lanes: Vec<String> = work.try_get("lanes")?;
    let reasons: Vec<Lane> = lanes
        .iter()
        .map(|s| {
            if s == "changed" {
                Lane::Changed
            } else {
                Lane::Aging
            }
        })
        .collect();
    report::submission_for_work(&input, &primary, &reasons)?;
    let mut paths: Vec<String> = input
        .evidence
        .iter()
        .chain(input.findings.iter().flat_map(|f| f.evidence.iter()))
        .map(|e| e.pin.path.clone())
        .chain(std::iter::once(primary.path.clone()))
        .collect();
    paths.sort();
    paths.dedup();
    let sources: Vec<(String,i32,bool)> = sqlx::query_as("SELECT path,revision,deleted_at IS NULL AND kind='concept' FROM documents WHERE catalog_id=$1 AND path=ANY($2) ORDER BY path FOR SHARE")
        .bind(catalog_id).bind(&paths).fetch_all(&mut *tx).await?;
    let locked_current = |pin: &RevisionPin| {
        sources
            .iter()
            .any(|(path, revision, live)| *live && *path == pin.path && *revision == pin.revision)
    };
    let historical = !locked_current(&primary);
    // A path inserted after the lock statement snapshot is not protected by
    // that statement. Never certify a pin absent from the locked current set.
    let mut outdated = historical
        || input
            .evidence
            .iter()
            .chain(input.findings.iter().flat_map(|f| f.evidence.iter()))
            .any(|item| !locked_current(&item.pin));
    outdated |= evidence::validate(&mut tx, catalog_id, &input.evidence).await?;
    for finding in &input.findings {
        outdated |= evidence::validate(&mut tx, catalog_id, &finding.evidence).await?;
    }
    let outcome = if historical {
        WorkOutcome::Superseded
    } else {
        input.outcome
    };
    store_evidence(&mut tx, catalog_id, work_id, None, &input.evidence).await?;
    let mut finding_ids = Vec::new();
    for finding in &input.findings {
        let id = new_id()?;
        sqlx::query("INSERT INTO maintenance_findings (id,run_id,work_id,catalog_id,category,assertion,follow_up,uncertainty,relationship,external,created_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)")
            .bind(&id).bind(&run_id).bind(work_id).bind(catalog_id).bind(enum_text(finding.category)?)
            .bind(&finding.assertion).bind(&finding.follow_up).bind(&finding.uncertainty).bind(enum_text(finding.relationship)?)
            .bind(json!(finding.external)).bind(now).execute(&mut *tx).await?;
        store_evidence(&mut tx, catalog_id, work_id, Some(&id), &finding.evidence).await?;
        finding_ids.push(id);
    }
    sqlx::query("UPDATE maintenance_work SET outcome=$3,assessment=$4,summary=$5,reported_tokens=$6,ended_at=$7,reason=$8 WHERE catalog_id=$1 AND id=$2")
        .bind(catalog_id).bind(work_id).bind(enum_text(outcome)?).bind(enum_text(input.assessment)?)
        .bind(&input.summary).bind(tokens).bind(now).bind(if historical { Some("source_changed") } else { outdated.then_some("evidence_outdated") }).execute(&mut *tx).await?;
    // Patch only this item's lanes on the locked current state, never a batch's
    // proposed cursor snapshot (another item may have completed since claim).
    if outcome == WorkOutcome::Completed {
        let group: Option<String> = work.try_get("aging_group")?;
        sqlx::query("UPDATE maintenance_state SET changed_cursor=CASE WHEN $2 THEN $3 ELSE changed_cursor END,expiry_cursor=CASE WHEN $4='expiry' THEN $3 ELSE expiry_cursor END,verification_cursor=CASE WHEN $4='verification' THEN $3 ELSE verification_cursor END,next_aging_group=CASE WHEN $4='expiry' THEN 'verification' WHEN $4='verification' THEN 'expiry' ELSE next_aging_group END,updated_at=$5 WHERE catalog_id=$1")
            .bind(catalog_id).bind(reasons.contains(&Lane::Changed)).bind(&primary.path).bind(group).bind(now).execute(&mut *tx).await?;
    }
    runs::complete_if_terminal(&mut tx, catalog_id, &run_id, now).await?;
    let result = Receipt {
        run_id,
        work_id: work_id.into(),
        outcome,
        finding_ids,
        outdated,
    };
    receipt(
        &mut tx,
        catalog_id,
        actor,
        "submit",
        idempotency_key,
        &payload,
        &result,
    )
    .await?;
    tx.commit().await?;
    Ok(result)
}
