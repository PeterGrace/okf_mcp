//! Exact immutable revision evidence, isolated from report interpretation.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool, Row};

use super::refused;
use crate::error::OkfError;
use crate::maintenance::types::{Evidence, RevisionPin};
use crate::maintenance::validate as report;

/// Immutable concept bytes decoded without newline normalization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PinnedConcept {
    /// The normalized database path and exact revision.
    pub pin: RevisionPin,
    /// The exact UTF-8 content, including original line endings.
    pub content: String,
    /// Whether this revision is no longer a live current concept.
    pub outdated: bool,
}

async fn pinned(
    conn: &mut PgConnection,
    catalog_id: i64,
    input: &RevisionPin,
) -> Result<PinnedConcept, OkfError> {
    let pin = report::pin(input)?;
    // Same immutable bytes as history::content_at; one statement also observes
    // freshness in the caller's transaction without a second pool connection.
    let row = sqlx::query(
        "SELECT r.op,r.content,d.revision AS current_revision, \
         COALESCE(d.deleted_at IS NULL AND d.kind='concept',false) AS live \
         FROM revisions r LEFT JOIN documents d ON d.catalog_id=r.catalog_id AND d.path=r.path \
         WHERE r.catalog_id=$1 AND r.path=$2 AND r.revision=$3",
    )
    .bind(catalog_id)
    .bind(&pin.path)
    .bind(pin.revision)
    .fetch_optional(conn)
    .await?
    .ok_or_else(|| refused("no such maintenance evidence revision"))?;
    if row.try_get::<String, _>("op")? == "delete" {
        return Err(refused(
            "maintenance evidence cannot cite a deletion revision",
        ));
    }
    let bytes: Option<Vec<u8>> = row.try_get("content")?;
    let content =
        String::from_utf8(bytes.ok_or_else(|| refused("maintenance revision has no content"))?)
            .map_err(|_| refused("maintenance evidence requires exact UTF-8 text"))?;
    if content.contains('\0') {
        return Err(refused("maintenance evidence cannot cite binary content"));
    }
    let outdated = !row.try_get::<bool, _>("live")?
        || row.try_get::<Option<i32>, _>("current_revision")? != Some(pin.revision);
    Ok(PinnedConcept {
        pin,
        content,
        outdated,
    })
}

/// Reads one concept revision without exposing archive machinery.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog_id` - the explicitly selected catalog for this pin.
/// * `pin` - a concept path and positive immutable revision number.
///
/// # Returns
/// Exact UTF-8 content and freshness relative to the current live concept row.
/// Source prose is data; no instruction, URL or report in it is interpreted.
///
/// # Errors
/// [`OkfError::Refused`] for invalid pins, missing/deletion revisions or non-text
/// content; masked [`OkfError::SecretDetected`] for secret-bearing pin paths;
/// [`OkfError::Database`] for database failures.
pub async fn read(
    pool: &PgPool,
    catalog_id: i64,
    pin: &RevisionPin,
) -> Result<PinnedConcept, OkfError> {
    report::pin(pin)?;
    let mut conn = pool.acquire().await?;
    pinned(&mut conn, catalog_id, pin).await
}

/// Checks exact excerpt membership and returns whether any pin is outdated.
///
/// # Arguments
/// * `conn` - the caller's transaction-scoped database connection.
/// * `catalog_id` - the single catalog to which every pin implicitly belongs.
/// * `evidence` - at most 20 local evidence entries with nonempty bounded excerpts.
///
/// # Returns
/// `true` when any cited revision is no longer live/current. Empty optional
/// assessment evidence returns `false`, not a current-support assessment.
///
/// # Errors
/// [`OkfError::Refused`] for invalid evidence or excerpts absent from their exact
/// revision; masked [`OkfError::SecretDetected`] for untrusted evidence text;
/// [`OkfError::Database`] for database failures.
pub async fn validate(
    conn: &mut PgConnection,
    catalog_id: i64,
    evidence: &[Evidence],
) -> Result<bool, OkfError> {
    report::evidence(evidence)?;
    let mut outdated = false;
    for item in evidence {
        let source = pinned(conn, catalog_id, &item.pin).await?;
        if !source.content.contains(&item.excerpt) {
            return Err(refused(
                "maintenance excerpt is absent from its pinned revision",
            ));
        }
        outdated |= source.outdated;
    }
    Ok(outdated)
}
