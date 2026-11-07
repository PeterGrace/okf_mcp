//! Tool-restricted Claude subprocesses, not an OS sandbox or egress firewall.
//!
//! Byte limits and deadlines are local hard limits. The requested spend reservation
//! is a stopping threshold, not an in-flight billing/token guarantee. No retries or
//! model-based repair are performed. Preflight does not prove paid-envelope support.

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{path::PathBuf, process::Stdio, time::Duration};

use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
    sync::Mutex,
    task::JoinSet,
    time::{timeout_at, Instant},
};
use tokio_util::sync::CancellationToken;

use super::{
    config::{RunnerConfig, Secrets},
    error::RunnerError,
    proposal::{decode_proposal, proposal_schema},
    types::{ModelRequest, ModelResult, ModelUsage},
};

const STDOUT_CAP: usize = 1_048_576;
const STDERR_CAP: usize = 65_536;
const PROMPT_CAP: usize = 131_072;
const CHILD_TIMEOUT: Duration = Duration::from_secs(90);
// Includes preflight and separate adapter instances, not just calls on one object.
static PROCESS_LOCK: Mutex<()> = Mutex::const_new(());
const FLAGS: &[&str] = &[
    "--print",
    "--output-format",
    "--json-schema",
    "--tools",
    "--strict-mcp-config",
    "--mcp-config",
    "--bare",
    "--restricted",
    "--disable-slash-commands",
    "--no-session-persistence",
    "--no-chrome",
    "--setting-sources",
    "--model",
    "--max-budget-usd",
];

/// A single, non-retrying inference boundary with explicit cancellation.
pub trait Inference: Send + Sync {
    fn infer(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> impl std::future::Future<Output = Result<ModelResult, RunnerError>> + Send;
}

/// Trusted configuration and dedicated provider credential; deliberately not Debug.
pub struct ClaudeAdapter {
    binary: PathBuf,
    model: String,
    reservation: u64,
    provider_key: String,
}

impl ClaudeAdapter {
    /// Requires both live-consent flags and a dedicated key; never uses OAuth.
    /// The caller must also run `preflight_cli` before starting live work.
    pub fn new(config: &RunnerConfig, secrets: &Secrets) -> Result<Self, RunnerError> {
        config.validate_consent()?;
        let key = secrets.anthropic_api_key();
        if key.trim().is_empty() || key.contains('\0') || config.model.contains('\0') {
            return Err(RunnerError::Config);
        }
        Ok(Self {
            binary: resolve_binary(config)?,
            model: config.model.clone(),
            reservation: config.invocation_microusd,
            provider_key: key.to_owned(),
        })
    }
}

impl Inference for ClaudeAdapter {
    async fn infer(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ModelResult, RunnerError> {
        let (budget, fractional) =
            decimal_microusd(&request.budget_usd).map_err(|_| RunnerError::Config)?;
        if request.model != self.model
            || budget != self.reservation
            || fractional
            || request.prompt.len() > PROMPT_CAP
        {
            return Err(RunnerError::Config);
        }
        let budget = format!(
            "{}.{:06}",
            self.reservation / 1_000_000,
            self.reservation % 1_000_000
        );
        let args = vec![
            "--print".into(),
            "--output-format".into(),
            "json".into(),
            "--json-schema".into(),
            proposal_schema().to_string(),
            "--tools".into(),
            "".into(),
            "--strict-mcp-config".into(),
            "--mcp-config".into(),
            "{\"mcpServers\":{}}".into(),
            "--bare".into(),
            "--restricted".into(),
            "--disable-slash-commands".into(),
            "--no-session-persistence".into(),
            "--no-chrome".into(),
            "--setting-sources".into(),
            "".into(),
            "--model".into(),
            self.model.clone(),
            "--max-budget-usd".into(),
            budget,
        ];
        let bytes = run_process(
            self.binary.clone(),
            args,
            Some(self.provider_key.clone()),
            request.prompt.into_bytes(),
            cancel,
            CHILD_TIMEOUT,
        )
        .await?;
        parse_envelope(&bytes)
    }
}

fn resolve_binary(config: &RunnerConfig) -> Result<PathBuf, RunnerError> {
    config.validate()?;
    let binary = std::fs::canonicalize(&config.claude_binary).map_err(|_| RunnerError::Config)?;
    if !binary.is_file() {
        return Err(RunnerError::Config);
    }
    Ok(binary)
}

/// Inspect only the configured binary's version/help, without a key or inference.
pub async fn preflight_cli(config: &RunnerConfig) -> Result<(), RunnerError> {
    preflight_cli_with_cancel(config, CancellationToken::new()).await
}

/// Cancel readiness by signalling this token and awaiting this future to completion.
/// Completion joins the process worker after kill/wait and pipe cleanup. Dropping
/// the future requests cleanup but cannot await it or guarantee reaping at exit.
pub async fn preflight_cli_with_cancel(
    config: &RunnerConfig,
    cancel: CancellationToken,
) -> Result<(), RunnerError> {
    if cancel.is_cancelled() {
        return Err(RunnerError::Cancelled);
    }
    let binary = resolve_binary(config)?;
    let version = run_process(
        binary.clone(),
        vec!["--version".into()],
        None,
        vec![],
        cancel.clone(),
        CHILD_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        RunnerError::Cancelled => RunnerError::Cancelled,
        _ => RunnerError::Contract,
    })?;
    if cancel.is_cancelled() {
        return Err(RunnerError::Cancelled);
    }
    let version = std::str::from_utf8(&version).map_err(|_| RunnerError::Contract)?;
    let version = version
        .strip_suffix("\r\n")
        .or_else(|| version.strip_suffix('\n'))
        .unwrap_or(version);
    if version != config.expected_claude_version {
        return Err(RunnerError::Contract);
    }
    let help = run_process(
        binary,
        vec!["--help".into()],
        None,
        vec![],
        cancel.clone(),
        CHILD_TIMEOUT,
    )
    .await
    .map_err(|error| match error {
        RunnerError::Cancelled => RunnerError::Cancelled,
        _ => RunnerError::Contract,
    })?;
    if cancel.is_cancelled() {
        return Err(RunnerError::Cancelled);
    }
    let help = std::str::from_utf8(&help).map_err(|_| RunnerError::Contract)?;
    let tokens: Vec<_> = help
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        .collect();
    if FLAGS.iter().any(|flag| !tokens.contains(flag)) {
        return Err(RunnerError::Contract);
    }
    Ok(())
}

async fn read_bounded(
    mut pipe: impl AsyncRead + Unpin,
    cap: usize,
    retain: bool,
) -> Result<Vec<u8>, RunnerError> {
    let mut bytes = Vec::new();
    let mut count = 0;
    let mut buf = [0; 8192];
    loop {
        // Read at most one byte beyond the limit, never allocate the overflow.
        let limit = buf.len().min(cap - count + 1);
        let n = pipe
            .read(&mut buf[..limit])
            .await
            .map_err(|_| RunnerError::Child)?;
        if n == 0 {
            return Ok(bytes);
        }
        if n > cap - count {
            return Err(RunnerError::Child);
        }
        count += n;
        if retain {
            bytes.extend_from_slice(&buf[..n]);
        }
    }
}

#[cfg(unix)]
fn private_directory() -> Result<tempfile::TempDir, RunnerError> {
    tempfile::Builder::new()
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in("/tmp")
        .map_err(|_| RunnerError::Io)
}

#[cfg(not(unix))]
fn private_directory() -> Result<tempfile::TempDir, RunnerError> {
    // No equivalent private-directory policy has been established here.
    Err(RunnerError::Config)
}

async fn run_process(
    binary: PathBuf,
    args: Vec<String>,
    key: Option<String>,
    prompt: Vec<u8>,
    cancel: CancellationToken,
    timeout: Duration,
) -> Result<Vec<u8>, RunnerError> {
    // A dropped caller signals the worker but cannot join it. Callers that own
    // shutdown must cancel and await this future before tearing down the runtime.
    let local_cancel = cancel.child_token();
    let _cancel_on_drop = local_cancel.clone().drop_guard();
    let deadline = Instant::now() + timeout;
    tokio::spawn(async move {
        let _lock = tokio::select! {
            biased;
            _ = local_cancel.cancelled() => return Err(RunnerError::Cancelled),
            lock = timeout_at(deadline, PROCESS_LOCK.lock()) => lock.map_err(|_| RunnerError::Child)?,
        };
        // Do not consult inherited TMPDIR, HOME, PATH, locale or configuration.
        let private = private_directory()?;
        let mut command = Command::new(binary);
        command.args(args).current_dir(private.path()).env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", private.path()).env("TMPDIR", private.path()).env("LANG", "C.UTF-8")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        if let Some(key) = key { command.env("ANTHROPIC_API_KEY", key); }
        let mut child = command.spawn().map_err(|_| RunnerError::Child)?;
        let mut tasks = JoinSet::new();
        let stdout = child.stdout.take().ok_or(RunnerError::Child)?;
        let stderr = child.stderr.take().ok_or(RunnerError::Child)?;
        let mut stdin = child.stdin.take().ok_or(RunnerError::Child)?;
        tasks.spawn(async move { read_bounded(stdout, STDOUT_CAP, true).await.map(Some) });
        tasks.spawn(async move { read_bounded(stderr, STDERR_CAP, false).await.map(|_| None) });
        tasks.spawn(async move {
            stdin.write_all(&prompt).await.map_err(|_| RunnerError::Child)?;
            stdin.shutdown().await.map_err(|_| RunnerError::Child)?;
            Ok(None)
        });
        let operation = async {
            let mut output = None;
            let mut status = None;
            while !tasks.is_empty() || status.is_none() {
                tokio::select! {
                    result = child.wait(), if status.is_none() => {
                        let exit = result.map_err(|_| RunnerError::Child)?;
                        if !exit.success() { return Err(RunnerError::Child); }
                        status = Some(exit);
                    }
                    result = tasks.join_next(), if !tasks.is_empty() => {
                        if let Some(bytes) = result.ok_or(RunnerError::Child)?.map_err(|_| RunnerError::Child)?? { output = Some(bytes); }
                    }
                }
            }
            output.ok_or(RunnerError::Child)
        };
        let result = tokio::select! {
            biased;
            _ = local_cancel.cancelled() => Err(RunnerError::Cancelled),
            result = timeout_at(deadline, operation) => result.unwrap_or(Err(RunnerError::Child)),
        };
        if result.is_err() {
            // Only this owned child is signalled. Never kill a process group or PID
            // discovered through user-controlled output.
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        result
    }).await.map_err(|_| RunnerError::Child)?
}

// Envelopes can add unrelated metadata; the nested Proposal remains closed.
#[derive(Deserialize)]
struct Envelope {
    #[serde(rename = "type")]
    kind: String,
    subtype: String,
    is_error: bool,
    structured_output: Option<Box<RawValue>>,
    total_cost_usd: Option<Box<RawValue>>,
    usage: Option<Box<RawValue>>,
}
#[derive(Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
    cache_creation_input_tokens: u64,
    cache_read_input_tokens: u64,
}

// Derived struct deserializers also accept positional sequences. Require the JSON
// object token before decoding, preserving RawValue lexemes and duplicate checks.
fn parse_object<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    error: RunnerError,
) -> Result<T, RunnerError> {
    if bytes.iter().find(|byte| !byte.is_ascii_whitespace()) != Some(&b'{') {
        return Err(error);
    }
    serde_json::from_slice(bytes).map_err(|_| error)
}

/// Parse only the target success envelope; never fall back to text or zero usage.
pub fn parse_envelope(bytes: &[u8]) -> Result<ModelResult, RunnerError> {
    if bytes.len() > STDOUT_CAP {
        return Err(RunnerError::Child);
    }
    let envelope: Envelope = parse_object(bytes, RunnerError::UnknownAccounting)?;
    if envelope.kind != "result" || envelope.subtype != "success" || envelope.is_error {
        return Err(RunnerError::Child);
    }
    let usage: Usage = parse_object(
        envelope
            .usage
            .ok_or(RunnerError::UnknownAccounting)?
            .get()
            .as_bytes(),
        RunnerError::UnknownAccounting,
    )?;
    usage
        .input_tokens
        .checked_add(usage.output_tokens)
        .and_then(|v| v.checked_add(usage.cache_creation_input_tokens))
        .and_then(|v| v.checked_add(usage.cache_read_input_tokens))
        .ok_or(RunnerError::UnknownAccounting)?;
    let cost = envelope
        .total_cost_usd
        .ok_or(RunnerError::UnknownAccounting)?;
    let (cost_microusd, _) = decimal_microusd(cost.get())?;
    let proposal_bytes = envelope
        .structured_output
        .ok_or(RunnerError::InvalidProposal)?;
    let proposal = decode_proposal(proposal_bytes.get().as_bytes())?;
    Ok(ModelResult {
        proposal,
        usage: ModelUsage {
            input: usage.input_tokens,
            output: usage.output_tokens,
            cache_creation: usage.cache_creation_input_tokens,
            cache_read: usage.cache_read_input_tokens,
            cost_microusd,
        },
    })
}

// Work on the original JSON number lexeme, never serde_json::Value/f64. Bound
// significand and exponent to 128; excessive precision is unknown, not rounded away.
// The bool reports a fractional microUSD so reservations can require exact equality.
fn decimal_microusd(text: &str) -> Result<(u64, bool), RunnerError> {
    let unknown = RunnerError::UnknownAccounting;
    if text.is_empty() || text.len() > 140 || !text.as_bytes()[0].is_ascii_digit() {
        return Err(unknown);
    }
    let _: Box<RawValue> = serde_json::from_str(text).map_err(|_| unknown)?;
    let (mantissa, exponent) = match text.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().map_err(|_| unknown)?),
        None => (text, 0),
    };
    if exponent.abs_diff(0) > 128 {
        return Err(unknown);
    }
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    if digits.len() > 128 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(unknown);
    }
    let scale = exponent + 6 - fraction.len() as i32;
    let (integer, rounded) = if scale < 0 {
        let cut = digits.len().saturating_sub((-scale) as usize);
        (&digits[..cut], digits[cut..].bytes().any(|b| b != b'0'))
    } else {
        (digits.as_str(), false)
    };
    let mut value = 0_u64;
    for digit in integer.bytes() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((digit - b'0') as u64))
            .ok_or(unknown)?;
    }
    for _ in 0..scale.max(0) {
        value = value.checked_mul(10).ok_or(unknown)?;
    }
    if rounded {
        value = value.checked_add(1).ok_or(unknown)?;
    }
    Ok((value, rounded))
}

#[cfg(all(test, target_os = "linux"))]
#[path = "claude_tests.rs"]
mod tests;
