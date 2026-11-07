//! Runner contracts, including pure proposal DTOs for the later validator.

use serde::{Deserialize, Serialize};

use crate::maintenance::types::{Category, Lane, Relationship, RevisionPin};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub origin: String,
    pub endpoint: String,
    pub catalog: String,
    pub credential_sha256: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowedTool {
    Search,
    GetConcept,
    History,
    Claim,
    Renew,
    ReadRevision,
    Submit,
    Close,
    GetRun,
}

impl AllowedTool {
    /// Only these nine server tools may be requested by the runner.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Search => "search",
            Self::GetConcept => "get_concept",
            Self::History => "history",
            Self::Claim => "maintenance_claim",
            Self::Renew => "maintenance_renew",
            Self::ReadRevision => "maintenance_read_revision",
            Self::Submit => "maintenance_submit",
            Self::Close => "maintenance_close",
            Self::GetRun => "maintenance_get_run",
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolRequest {
    pub tool: AllowedTool,
    pub args: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub id: String,
    pub pin: RevisionPin,
    pub text: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Packet {
    pub work_id: String,
    pub reasons: Vec<Lane>,
    pub primary_id: String,
    pub sources: Vec<Source>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelRequest {
    pub prompt: String,
    pub model: String,
    pub budget_usd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelUsage {
    pub input: u64,
    pub output: u64,
    pub cache_creation: u64,
    pub cache_read: u64,
    pub cost_microusd: u64,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelResult {
    pub proposal: Proposal,
    pub usage: ModelUsage,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunSummary {
    pub run_id: Option<String>,
    pub reported: u32,
    pub inconclusive: u32,
    pub failed: u32,
    pub skipped: u32,
    pub invocations: u32,
    pub accounting_known: bool,
    pub reason: String,
}

impl RunSummary {
    /// Operational status, distinct from preflight error classification.
    pub fn exit_code(&self) -> u8 {
        if self.accounting_known
            && self.failed == 0
            && self.skipped == 0
            && matches!(self.reason.as_str(), "completed" | "empty")
        {
            0
        } else {
            4
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Proposal {
    pub summary: String,
    pub findings: Vec<ProposedFinding>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedFinding {
    pub category: Category,
    pub assertion: String,
    pub follow_up: String,
    pub uncertainty: String,
    pub relationship: Relationship,
    pub evidence: Vec<ProposedEvidence>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProposedEvidence {
    pub source_id: String,
    pub excerpt: String,
}
