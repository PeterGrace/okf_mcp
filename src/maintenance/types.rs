//! Shared request and response shapes for maintenance operations.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Actor {
    pub key_id: String,
    pub actor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RevisionPin {
    pub path: String,
    #[schemars(range(min = 1))]
    pub revision: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Lane {
    Changed,
    Aging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgingGroup {
    Expiry,
    Verification,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkOutcome {
    Completed,
    Inconclusive,
    Failed,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Assessment {
    SupportedCurrent,
    Outdated,
    Unverifiable,
    NotAssessed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    SuggestedLink,
    PossibleDuplicate,
    ConflictingAssertions,
    PotentiallyOutdatedInformation,
    UnverifiableClaim,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Relationship {
    DirectStatement,
    Inference,
    ExternalObservation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    Open,
    AcceptedForFollowUp,
    Dismissed,
    NeedsInput,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Evidence {
    pub pin: RevisionPin,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ExternalObservation {
    pub resource: String,
    pub excerpt: String,
    pub attribution: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindingInput {
    pub category: Category,
    pub assertion: String,
    pub follow_up: String,
    pub uncertainty: String,
    pub relationship: Relationship,
    pub evidence: Vec<Evidence>,
    pub external: Vec<ExternalObservation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Submission {
    #[schemars(schema_with = "submission_outcome_schema")]
    pub outcome: WorkOutcome,
    pub assessment: Assessment,
    pub summary: String,
    pub evidence: Vec<Evidence>,
    pub findings: Vec<FindingInput>,
    pub reported_tokens: Option<u64>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Lease {
    pub run_id: String,
    pub work_id: String,
    pub token: String,
    pub pin: RevisionPin,
    pub reasons: Vec<Lane>,
    pub expires_at: String,
}

impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lease")
            .field("run_id", &self.run_id)
            .field("work_id", &self.work_id)
            .field("token", &"[REDACTED]")
            .field("pin", &self.pin)
            .field("reasons", &self.reasons)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ClaimResult {
    pub run_id: String,
    pub leases: Vec<Lease>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub run_id: String,
    pub work_id: String,
    pub outcome: WorkOutcome,
    pub finding_ids: Vec<String>,
    pub outdated: bool,
}

fn submission_outcome_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"type": "string", "enum": ["completed", "inconclusive", "failed"]})
}

/// A 128-bit opaque identifier, rendered as lowercase hexadecimal.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn new_id() -> Result<String, OkfError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod lease_debug_tests {
    use super::*;

    #[test]
    fn maintenance_lease_debug_redacts_token_without_changing_wire() {
        let wire = serde_json::json!({"run_id":"r","work_id":"w","token":"sensitive-lease-token","pin":{"path":"/x.md","revision":1},"reasons":["changed"],"expires_at":"expiry"});
        let lease: Lease = serde_json::from_value(wire.clone()).unwrap();
        assert!(!format!("{lease:?}").contains("sensitive-lease-token"));
        assert!(!format!("{lease:#?}").contains("sensitive-lease-token"));
        let result = ClaimResult {
            run_id: "r".into(),
            leases: vec![lease.clone()],
        };
        assert!(!format!("{result:?}").contains("sensitive-lease-token"));
        assert_eq!(serde_json::to_value(lease).unwrap(), wire);
    }
}
