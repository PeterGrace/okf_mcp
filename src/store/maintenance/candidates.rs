//! Authoritative maintenance candidates from published rows and revision-scoped history.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use sqlx::{PgConnection, Row};

use crate::error::OkfError;
use crate::maintenance::select::{Candidate, Cursors};
use crate::maintenance::types::RevisionPin;
use crate::okf::doc::Doc;
use crate::okf::paths::is_concept;
use crate::okf::timestamp::parse_iso;
use crate::okf::yaml::Value;

fn timestamp(value: &Value) -> Option<DateTime<Utc>> {
    value
        .as_str()
        .and_then(parse_iso)
        .and_then(|t| t.aware())
        .map(|t| t.with_timezone(&Utc))
}

fn metadata(
    path: &str,
    content: &[u8],
    now: DateTime<Utc>,
) -> (bool, Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let Some(data) = std::str::from_utf8(content)
        .ok()
        .and_then(|text| Doc::from_text(path, text).parse().ok())
    else {
        return (false, None, None);
    };
    let deprecated = data.get("status").and_then(Value::as_str) == Some("deprecated");
    let stale = data.get("stale_after").and_then(timestamp);
    let verified = data.get("verified").and_then(|v| match v {
        Value::Seq(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::Map(fields) => fields.get("at").and_then(timestamp).filter(|at| *at <= now),
                _ => None,
            })
            .max(),
        _ => None,
    });
    (deprecated, stale, verified)
}

/// Loads live concept revisions and their maintenance scheduling facts.
///
/// # Errors
/// Database errors are propagated; malformed frontmatter is ignored per row.
pub async fn load(
    conn: &mut PgConnection,
    catalog_id: i64,
    now: DateTime<Utc>,
) -> Result<Vec<Candidate>, OkfError> {
    let rows = sqlx::query("SELECT path, revision, content, updated_at FROM documents WHERE catalog_id=$1 AND deleted_at IS NULL AND kind='concept' ORDER BY path")
        .bind(catalog_id).fetch_all(&mut *conn).await?;
    // Transfer only historical scheduling hints and current-revision work details.
    // Attempts are aggregated across revisions so edits cannot reset aging priority.
    let hint_paths: Vec<String> = rows
        .iter()
        .filter_map(|row| {
            let path: String = row.get("path");
            let content: Vec<u8> = row.get("content");
            let (deprecated, _, verified) = metadata(&path, &content, now);
            (!deprecated
                && verified.is_some_and(|at| at >= row.get::<DateTime<Utc>, _>("updated_at")))
            .then_some(path)
        })
        .collect();
    let historical: Vec<(String, Vec<u8>)> = sqlx::query_as(
        "SELECT r.path,r.content FROM revisions r JOIN documents d ON d.catalog_id=r.catalog_id AND d.path=r.path WHERE r.catalog_id=$1 AND r.path=ANY($2) AND r.revision<d.revision AND r.content IS NOT NULL",
    ).bind(catalog_id).bind(&hint_paths).fetch_all(&mut *conn).await?;
    let historical_hints: HashSet<_> = historical
        .into_iter()
        .filter_map(|(path, bytes)| metadata(&path, &bytes, now).2.map(|hint| (path, hint)))
        .collect();
    let attempts: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT path,max(attempted_at) FROM maintenance_work WHERE catalog_id=$1 GROUP BY path",
    )
    .bind(catalog_id)
    .fetch_all(&mut *conn)
    .await?;
    let attempts: HashMap<_, _> = attempts.into_iter().collect();
    let history = sqlx::query("SELECT w.path, lanes, outcome, assessment, attempted_at, ended_at, reason, EXISTS (SELECT 1 FROM maintenance_evidence e WHERE e.catalog_id=w.catalog_id AND e.work_id=w.id AND e.finding_id IS NULL AND e.path=w.path AND e.revision=w.revision) AND NOT EXISTS (SELECT 1 FROM maintenance_evidence e LEFT JOIN documents d ON d.catalog_id=e.catalog_id AND d.path=e.path WHERE e.catalog_id=w.catalog_id AND e.work_id=w.id AND (d.revision IS DISTINCT FROM e.revision OR d.deleted_at IS NOT NULL OR d.kind IS DISTINCT FROM 'concept')) AND NOT EXISTS (SELECT 1 FROM maintenance_findings f WHERE f.catalog_id=w.catalog_id AND f.work_id=w.id AND f.category='potentially_outdated_information') AS effective_evidence FROM maintenance_work w JOIN documents d ON d.catalog_id=w.catalog_id AND d.path=w.path AND d.revision=w.revision WHERE w.catalog_id=$1 AND d.deleted_at IS NULL AND d.kind='concept' ORDER BY attempted_at DESC, w.id DESC")
        .bind(catalog_id).fetch_all(&mut *conn).await?;
    let mut histories: HashMap<String, Vec<_>> = HashMap::new();
    for work in history {
        histories
            .entry(work.try_get("path")?)
            .or_default()
            .push(work);
    }
    let mut candidates = Vec::new();
    for row in rows {
        let path: String = row.try_get("path")?;
        if !path.ends_with(".md") || !is_concept(&path) {
            continue;
        }
        let revision: i32 = row.try_get("revision")?;
        let content: Vec<u8> = row.try_get("content")?;
        let (deprecated, stale_after, current_verified) = metadata(&path, &content, now);
        if deprecated {
            continue;
        }
        // A carried-forward stamp only belongs to this revision when its timestamp
        // follows the revision's publication. Otherwise locate the historical source.
        let updated_at: DateTime<Utc> = row.try_get("updated_at")?;
        let mut legacy_verified = current_verified.filter(|at| *at >= updated_at);
        if legacy_verified.is_some_and(|hint| historical_hints.contains(&(path.clone(), hint))) {
            legacy_verified = None;
        }
        let history = histories.remove(&path).unwrap_or_default();
        let last_attempt = attempts.get(&path).copied();
        let mut candidate = Candidate {
            pin: RevisionPin { path, revision },
            edited_at: updated_at,
            stale_after,
            legacy_verified,
            changed_done: false,
            last_attempt,
            last_aging_completed: None,
            reported_supported: None,
            retry_at: None,
            claimed: false,
        };
        let mut latest_terminal: Option<(DateTime<Utc>, Option<DateTime<Utc>>)> = None;
        for work in history {
            let lanes: Vec<String> = work.try_get("lanes")?;
            let outcome: Option<String> = work.try_get("outcome")?;
            let ended_at: Option<DateTime<Utc>> = work.try_get("ended_at")?;
            let reason: Option<String> = work.try_get("reason")?;
            let assessment: Option<String> = work.try_get("assessment")?;
            candidate.claimed |= outcome.is_none();
            if outcome.as_deref() == Some("completed") {
                if lanes.iter().any(|lane| lane == "changed") {
                    candidate.changed_done = true;
                }
                if lanes.iter().any(|lane| lane == "aging") {
                    if let Some(at) = ended_at {
                        candidate.last_aging_completed =
                            Some(candidate.last_aging_completed.map_or(at, |old| old.max(at)));
                        if candidate.last_aging_completed == Some(at) {
                            candidate.reported_supported = (assessment.as_deref()
                                == Some("supported_current")
                                && reason.as_deref() != Some("evidence_outdated")
                                && work.try_get::<bool, _>("effective_evidence")?)
                            .then_some((revision, at));
                        }
                    }
                }
            }
            if outcome.is_some() {
                if let Some(at) = ended_at {
                    let retry = (matches!(outcome.as_deref(), Some("failed" | "inconclusive"))
                        && !matches!(reason.as_deref(), Some("lease_expired" | "run_closed")))
                    .then_some(at + Duration::seconds(86_400));
                    if latest_terminal.is_none_or(|(last, _)| at > last) {
                        latest_terminal = Some((at, retry));
                    }
                }
            }
        }
        candidate.retry_at = latest_terminal.and_then(|(_, retry)| retry);
        candidates.push(candidate);
    }
    Ok(candidates)
}

/// Reads the persisted scheduling cursors, or initial positions before the first claim.
///
/// # Errors
/// Database errors are propagated.
pub async fn cursors(conn: &mut PgConnection, catalog_id: i64) -> Result<Cursors, OkfError> {
    let row = sqlx::query("SELECT changed_cursor, expiry_cursor, verification_cursor, next_aging_group FROM maintenance_state WHERE catalog_id=$1")
        .bind(catalog_id).fetch_optional(conn).await?;
    Ok(if let Some(row) = row {
        Cursors {
            changed: row.try_get("changed_cursor")?,
            expiry: row.try_get("expiry_cursor")?,
            verification: row.try_get("verification_cursor")?,
            next_expiry: row.try_get::<String, _>("next_aging_group")? == "expiry",
        }
    } else {
        Cursors {
            changed: String::new(),
            expiry: String::new(),
            verification: String::new(),
            next_expiry: true,
        }
    })
}
