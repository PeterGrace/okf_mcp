//! Strict offline configuration and environment-only credentials.

use std::path::{Path, PathBuf};

use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::error::RunnerError;
use super::types::Binding;

#[derive(Clone, Serialize, Deserialize)]
#[serde(from = "ConfigInput", deny_unknown_fields)]
pub struct RunnerConfig {
    pub endpoint: String,
    pub catalog: String,
    pub claude_binary: PathBuf,
    pub expected_claude_version: String,
    pub model: String,
    pub state_dir: PathBuf,
    pub allow_model_upload: bool,
    pub acknowledge_inflight_budget_overshoot: bool,
    pub require_hard_billing_cap: bool,
    pub batch_size: u32,
    pub max_invocations: u32,
    pub max_run_seconds: u64,
    pub invocation_microusd: u64,
    pub run_microusd: u64,
    pub observed_token_stop: u64,
}

// An omitted invocation limit depends on the selected batch; an explicit limit
// must never be silently clamped. Keep the optional value at the input boundary.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigInput {
    endpoint: String,
    catalog: String,
    claude_binary: PathBuf,
    expected_claude_version: String,
    model: String,
    state_dir: PathBuf,
    #[serde(default)]
    allow_model_upload: bool,
    #[serde(default)]
    acknowledge_inflight_budget_overshoot: bool,
    #[serde(default)]
    require_hard_billing_cap: bool,
    #[serde(default = "default_batch")]
    batch_size: u32,
    #[serde(default)]
    max_invocations: Option<u32>,
    #[serde(default = "default_run_seconds")]
    max_run_seconds: u64,
    #[serde(default = "default_invocation_microusd")]
    invocation_microusd: u64,
    #[serde(default = "default_run_microusd")]
    run_microusd: u64,
    #[serde(default = "default_token_stop")]
    observed_token_stop: u64,
}

fn default_batch() -> u32 {
    10
}
fn default_run_seconds() -> u64 {
    1_800
}
fn default_invocation_microusd() -> u64 {
    100_000
}
fn default_run_microusd() -> u64 {
    1_000_000
}
fn default_token_stop() -> u64 {
    100_000
}

impl From<ConfigInput> for RunnerConfig {
    fn from(input: ConfigInput) -> Self {
        Self {
            endpoint: input.endpoint,
            catalog: input.catalog,
            claude_binary: input.claude_binary,
            expected_claude_version: input.expected_claude_version,
            model: input.model,
            state_dir: input.state_dir,
            allow_model_upload: input.allow_model_upload,
            acknowledge_inflight_budget_overshoot: input.acknowledge_inflight_budget_overshoot,
            require_hard_billing_cap: input.require_hard_billing_cap,
            batch_size: input.batch_size,
            max_invocations: input.max_invocations.unwrap_or(10.min(input.batch_size)),
            max_run_seconds: input.max_run_seconds,
            invocation_microusd: input.invocation_microusd,
            run_microusd: input.run_microusd,
            observed_token_stop: input.observed_token_stop,
        }
    }
}

impl RunnerConfig {
    /// Read TOML without exposing parser errors (which can quote credentials).
    pub fn load(path: &Path) -> Result<Self, RunnerError> {
        let text = std::fs::read_to_string(path).map_err(|_| RunnerError::Io)?;
        let config: Self = toml::from_str(&text).map_err(|_| RunnerError::Config)?;
        config.validate()?;
        Ok(config)
    }

    /// Check static inputs only; valid offline configuration is not live consent.
    pub fn validate(&self) -> Result<(), RunnerError> {
        self.endpoint_url()?;
        if self.catalog.trim().is_empty()
            || self.model.trim().is_empty()
            || self.expected_claude_version.trim().is_empty()
            || !self.claude_binary.is_absolute()
            || !self.state_dir.is_absolute()
            || self.require_hard_billing_cap
            || !(1..=50).contains(&self.batch_size)
            || self.max_invocations == 0
            || self.max_invocations > self.batch_size
            || self.max_invocations > 50
            || !(1..=3_000).contains(&self.max_run_seconds)
            || self.invocation_microusd == 0
            || self.run_microusd == 0
            || self.invocation_microusd > self.run_microusd
            || self.observed_token_stop == 0
        {
            return Err(RunnerError::Config);
        }
        Ok(())
    }

    /// Required before any live operation; neither flag implies the other.
    pub fn validate_consent(&self) -> Result<(), RunnerError> {
        self.validate()?;
        if !self.allow_model_upload || !self.acknowledge_inflight_budget_overshoot {
            return Err(RunnerError::Config);
        }
        Ok(())
    }

    /// Bind local state to the entire endpoint, catalog and credential identity.
    pub fn binding(&self, token: &str) -> Result<Binding, RunnerError> {
        self.validate()?;
        if token.trim().is_empty() {
            return Err(RunnerError::Config);
        }
        let url = self.endpoint_url()?;
        let digest = Sha256::digest(token.as_bytes());
        Ok(Binding {
            origin: url.origin().ascii_serialization(),
            endpoint: url.to_string(),
            catalog: self.catalog.clone(),
            credential_sha256: digest.iter().map(|byte| format!("{byte:02x}")).collect(),
        })
    }

    fn endpoint_url(&self) -> Result<Url, RunnerError> {
        let url = Url::parse(&self.endpoint).map_err(|_| RunnerError::Config)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(RunnerError::Config);
        }
        Ok(url)
    }
}

/// Credentials are never deserialized, formatted or embedded in errors.
///
/// ```compile_fail
/// use okf_mcp::investigator::config::Secrets;
/// fn log_secret(secret: Secrets) { println!("{secret:?}"); }
/// ```
/// ```compile_fail
/// use okf_mcp::investigator::config::Secrets;
/// fn log_secret(secret: Secrets) { println!("{secret}"); }
/// ```
pub struct Secrets {
    token: String,
    anthropic_api_key: String,
}

impl Secrets {
    /// Load only the runner token and provider key, for network/live use.
    pub fn from_env() -> Result<Self, RunnerError> {
        fn required(name: &str) -> Result<String, RunnerError> {
            let value = std::env::var(name).map_err(|_| RunnerError::Config)?;
            if value.trim().is_empty() {
                return Err(RunnerError::Config);
            }
            Ok(value)
        }
        Ok(Self {
            token: required("OKF_MAINTENANCE_TOKEN")?,
            anthropic_api_key: required("ANTHROPIC_API_KEY")?,
        })
    }

    /// Explicit access for authenticated transport; never log this value.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Explicit access for the child environment; never log this value.
    pub fn anthropic_api_key(&self) -> &str {
        &self.anthropic_api_key
    }
}
