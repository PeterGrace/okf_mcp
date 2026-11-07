//! One report-only run: durable mutation replay, leased work and owned inference.
//!
//! `Err` is readiness or persistence failure. Operational interruptions return a
//! fixed-reason summary whose `exit_code()` is 4, including runtime protocol errors.
//! Reservations/observations are stopping thresholds, not hard billing guarantees.

use std::{collections::HashMap, sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use rmcp::model::CallToolResult;
use serde::de::DeserializeOwned;
use serde_json::{json, Value};
use tokio::{task::JoinHandle, time::Instant};
use tokio_util::sync::CancellationToken;

use super::{
    claude::{preflight_cli_with_cancel, Inference},
    client::CatalogClient,
    collect::{render_prompt, CollectStep, Collector},
    config::RunnerConfig,
    contract,
    error::RunnerError,
    journal::{Journal, JournalStore, PendingMutation, Phase, WorkState},
    proposal::to_submission,
    types::{AllowedTool, ModelRequest, ModelResult, ModelUsage, RunSummary, ToolRequest},
};
use crate::{
    clock::Clock,
    maintenance::types::{
        new_id, Assessment, ClaimResult, Lease, Receipt, Submission, WorkOutcome,
    },
};

const HEARTBEAT: Duration = Duration::from_secs(60);
const RPC: Duration = Duration::from_secs(10);
const SAFETY: i64 = 30;
const CHILD: Duration = Duration::from_secs(90);
// A mutation owns the slot through all three attempts and both backoffs.
const MUTATION_WINDOW: Duration = Duration::from_secs(33);

/// Run one bounded batch or recover its journal, without scheduling another run.
/// The caller supplies a locked store bound to its authenticated client credential.
/// No credential is read here. Inference must honor cancellation and reap its child.
pub async fn run_once<C: CatalogClient, I: Inference + 'static>(
    config: &RunnerConfig,
    client: &mut C,
    inference: Arc<I>,
    store: &JournalStore,
    clock: &dyn Clock,
    cancel: CancellationToken,
) -> Result<RunSummary, RunnerError> {
    run_once_with_deadline(
        config,
        client,
        inference,
        store,
        clock,
        cancel,
        Instant::now() + Duration::from_secs(config.max_run_seconds),
    )
    .await
}

/// Run with the caller's monotonic deadline, including time already spent on setup.
/// Cancellation still awaits owned readiness and inference child reaping.
pub async fn run_once_with_deadline<C: CatalogClient, I: Inference + 'static>(
    config: &RunnerConfig,
    client: &mut C,
    inference: Arc<I>,
    store: &JournalStore,
    clock: &dyn Clock,
    cancel: CancellationToken,
    local_deadline: Instant,
) -> Result<RunSummary, RunnerError> {
    config.validate_consent()?;
    if cancel.is_cancelled() {
        return Err(RunnerError::Cancelled);
    }
    let started = Instant::now();
    if started >= local_deadline {
        return Err(RunnerError::Budget);
    }
    let utc = clock.now();
    // Real executable readiness is required even with a fake Inference implementation.
    let readiness_cancel = cancel.child_token();
    let readiness = preflight_cli_with_cancel(config, readiness_cancel.clone());
    tokio::pin!(readiness);
    tokio::select! {
        biased;
        _ = cancel.cancelled() => {
            readiness_cancel.cancel();
            let _ = readiness.await;
            return Err(RunnerError::Cancelled);
        }
        _ = tokio::time::sleep_until(local_deadline) => {
            readiness_cancel.cancel();
            let _ = readiness.await;
            return Err(RunnerError::Budget);
        }
        result = &mut readiness => result?,
    }
    for attempt in 0..3 {
        if cancel.is_cancelled() {
            return Err(RunnerError::Cancelled);
        }
        let remaining = local_deadline
            .checked_duration_since(Instant::now())
            .ok_or(RunnerError::Budget)?;
        let result = tokio::select! {
            biased;
            _=cancel.cancelled()=>return Err(RunnerError::Cancelled),
            result=tokio::time::timeout(RPC.min(remaining),client.list_tools())=>result.unwrap_or(Err(RunnerError::Transport)),
        };
        if Instant::now() >= local_deadline {
            return Err(RunnerError::Budget);
        }
        match result {
            Ok(tools) => {
                contract::validate_tools(&tools)?;
                break;
            }
            Err(RunnerError::Transport) if attempt < 2 => {
                tokio::select! {
                    _=cancel.cancelled()=>return Err(RunnerError::Cancelled),
                    _=tokio::time::sleep_until(local_deadline)=>return Err(RunnerError::Budget),
                    _=tokio::time::sleep(Duration::from_secs(attempt+1))=>{},
                }
            }
            Err(error) => return Err(error),
        }
    }
    let fresh = store.fresh_journal()?;
    let endpoint = reqwest::Url::parse(&config.endpoint).map_err(|_| RunnerError::Config)?;
    if fresh.binding.endpoint != endpoint.as_str() || fresh.binding.catalog != config.catalog {
        return Err(RunnerError::Config);
    }
    let mut journal = store.load()?.unwrap_or_else(|| fresh.clone());
    if journal.terminal {
        // A previous directory-sync failure may have left this terminal state
        // visible but unconfirmed. Make it durable before starting another window.
        store.save(&journal)?;
        journal = fresh;
    }
    for work in &mut journal.work {
        if work.phase == Phase::InvocationStarted {
            work.failure = Some("unknown_accounting".into());
        }
    }
    store.save(&journal)?;
    let mut controller = Controller {
        config,
        client,
        store,
        clock,
        cancel,
        journal,
        started,
        local_deadline,
        utc,
        deadline: None,
        renew_at: HashMap::new(),
        sweep_at: None,
        child: None,
        cleanup: false,
    };
    let outcome = controller.execute(inference).await;
    match outcome {
        Ok(()) => Ok(controller.summary(if controller.journal.work.is_empty() {
            "empty"
        } else {
            "completed"
        })),
        Err(error) => {
            // Persist the run-level cause before cleanup can acknowledge valid
            // proposals or record a cancelled child as a separate work failure.
            let stop_saved = if persistence(error) {
                Ok(())
            } else {
                controller.stop(error)
            };
            // Never abort a JoinHandle: cancellation belongs to the process owner.
            // A result queued during a failed renewal still contributes known usage.
            if let Some(child) = controller.child.take() {
                child.cancel.cancel();
                let result = child
                    .task
                    .await
                    .unwrap_or(Err(RunnerError::UnknownAccounting));
                if !persistence(error) && stop_saved.is_ok() {
                    if let Err(save_error) = controller.record_model(child.index, result) {
                        if persistence(save_error) {
                            return Err(save_error);
                        }
                    }
                }
            }
            stop_saved?;
            if persistence(error) {
                return Err(error);
            }
            let reason = controller
                .journal
                .stop_reason
                .as_deref()
                .and_then(failure_kind)
                .unwrap_or(error);
            // No cleanup traffic after a permission/protocol/transport/clock failure.
            // Pending requests remain exact and durable for a later recovery.
            if matches!(
                error,
                RunnerError::Budget
                    | RunnerError::Cancelled
                    | RunnerError::UnknownAccounting
                    | RunnerError::InvalidProposal
                    | RunnerError::InvalidEvidence
                    | RunnerError::Child
            ) {
                controller.cleanup = true;
                if let Err(cleanup_error) = controller.finish(Some(reason)).await {
                    if persistence(cleanup_error) {
                        return Err(cleanup_error);
                    }
                }
            }
            Ok(controller.summary(kind(reason)))
        }
    }
}

struct Running {
    index: usize,
    cancel: CancellationToken,
    task: JoinHandle<Result<ModelResult, RunnerError>>,
    until: Instant,
}
struct Controller<'a, C> {
    config: &'a RunnerConfig,
    client: &'a mut C,
    store: &'a JournalStore,
    clock: &'a dyn Clock,
    cancel: CancellationToken,
    journal: Journal,
    started: Instant,
    local_deadline: Instant,
    utc: DateTime<Utc>,
    deadline: Option<DateTime<Utc>>,
    renew_at: HashMap<String, Instant>,
    sweep_at: Option<Instant>,
    child: Option<Running>,
    cleanup: bool,
}
impl<C: CatalogClient> Controller<'_, C> {
    fn save(&self) -> Result<(), RunnerError> {
        self.store.save(&self.journal)
    }

    fn stop(&mut self, error: RunnerError) -> Result<(), RunnerError> {
        if self.journal.stop_reason.is_none() {
            self.journal.stop_reason = Some(kind(error).into());
            self.save()?;
        }
        Ok(())
    }

    fn check(&self, recovering: bool) -> Result<(), RunnerError> {
        let elapsed = self.started.elapsed();
        let expected =
            self.utc + chrono::Duration::from_std(elapsed).map_err(|_| RunnerError::Budget)?;
        if (self.clock.now() - expected)
            .num_milliseconds()
            .unsigned_abs()
            > (SAFETY * 1000) as u64
        {
            return Err(RunnerError::Contract);
        }
        if Instant::now() >= self.local_deadline {
            return Err(RunnerError::Budget);
        }
        if !self.cleanup {
            if self.cancel.is_cancelled() {
                return Err(RunnerError::Cancelled);
            }
            if self
                .child
                .as_ref()
                .is_some_and(|c| !c.task.is_finished() && Instant::now() >= c.until)
            {
                return Err(RunnerError::Child);
            }
        }
        let now = self.clock.now() + chrono::Duration::seconds(SAFETY);
        if self.deadline.is_some_and(|end| now >= end) {
            return Err(RunnerError::Budget);
        }
        if !recovering {
            for lease in self.active_leases() {
                if timestamp(&lease.expires_at)? <= now {
                    return Err(RunnerError::LeaseLost);
                }
            }
        }
        Ok(())
    }
    fn active_leases(&self) -> impl Iterator<Item = &Lease> {
        self.journal.leases.iter().filter(|l| {
            self.journal.work.iter().any(|w| {
                w.work_id == l.work_id && !matches!(w.phase, Phase::Reported | Phase::Abandoned)
            })
        })
    }
    fn renewal_runway(&self) -> Duration {
        // Start the first sweep early enough to service queued leases. Later
        // sweeps have their own cadence; runway does not re-qualify fresh acks.
        (RPC * self.active_leases().count() as u32).min(HEARTBEAT - RPC)
    }
    fn next_renew(&self) -> Option<Instant> {
        let due = self
            .active_leases()
            .map(|l| {
                self.renew_at
                    .get(&l.work_id)
                    .copied()
                    .unwrap_or_else(Instant::now)
            })
            .min()?;
        let sweep = self.sweep_at.unwrap_or_else(|| {
            due.checked_sub(self.renewal_runway())
                .unwrap_or(self.started)
        });
        Some(due.min(sweep))
    }

    // None means an interrupted read, not an ambiguous mutation. Read retry state
    // stays with the caller while it services the due heartbeat.
    async fn attempt(
        &mut self,
        request: ToolRequest,
        recovering: bool,
        interrupt_read: bool,
    ) -> Result<Option<Value>, RunnerError> {
        self.check(recovering)?;
        let end = Instant::now() + RPC;
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.tick().await;
        let renew = if interrupt_read {
            self.next_renew()
        } else {
            None
        };
        let call = self.client.call(request.clone());
        tokio::pin!(call);
        // Snapshot the local controls so the client remains exclusively borrowed.
        let deadline = self.deadline;
        let leases: Vec<_> = self
            .journal
            .leases
            .iter()
            .filter(|l| {
                self.journal.work.iter().any(|w| {
                    w.work_id == l.work_id && !matches!(w.phase, Phase::Reported | Phase::Abandoned)
                })
            })
            .map(|l| timestamp(&l.expires_at))
            .collect::<Result<_, _>>()?;

        loop {
            tokio::select! {
                biased;
                _ = self.cancel.cancelled(), if !self.cleanup => return Err(RunnerError::Cancelled),
                _ = tokio::time::sleep_until(self.local_deadline) => return Err(RunnerError::Budget),
                _ = tokio::time::sleep_until(end) => return Err(RunnerError::Transport),
                value = &mut call => {
                    let value = value?;
                    return contract::result(request.tool, CallToolResult::structured(value), &self.config.catalog).map(Some);
                },
                _ = tick.tick() => {
                    let elapsed = self.started.elapsed();
                    let expected = self.utc + chrono::Duration::from_std(elapsed).map_err(|_| RunnerError::Budget)?;
                    let now = self.clock.now();
                    if (now-expected).num_milliseconds().unsigned_abs() > (SAFETY*1000) as u64 { return Err(RunnerError::Contract); }
                    if Instant::now() >= self.local_deadline { return Err(RunnerError::Budget); }
                    if !self.cleanup && self.child.as_ref().is_some_and(|c| !c.task.is_finished() && Instant::now() >= c.until) { return Err(RunnerError::Child); }
                    let margin = now + chrono::Duration::seconds(SAFETY);
                    if deadline.is_some_and(|d| margin >= d) { return Err(RunnerError::Budget); }
                    if !recovering && leases.iter().any(|d| margin >= *d) { return Err(RunnerError::LeaseLost); }
                    if renew.is_some_and(|d| Instant::now() >= d) { return Ok(None); }
                }
            }
        }
    }
    async fn delay(&self, seconds: u64, recovering: bool) -> Result<(), RunnerError> {
        for _ in 0..seconds {
            tokio::time::sleep(Duration::from_secs(1)).await;
            self.check(recovering)?;
        }
        Ok(())
    }
    async fn read(&mut self, request: ToolRequest) -> Result<Value, RunnerError> {
        let request = normalized(request, &self.config.catalog)?;
        let mut attempts = 0;
        loop {
            self.heartbeat(Duration::ZERO).await?;
            match self.attempt(request.clone(), false, true).await {
                Ok(Some(value)) => {
                    self.check(false)?;
                    return Ok(value);
                }
                Ok(None) => continue,
                Err(RunnerError::Transport) if attempts < 2 => {
                    attempts += 1;
                    self.delay(attempts, false).await?;
                }
                Err(error) => return Err(error),
            }
        }
    }
    fn prepare(&mut self, tool: AllowedTool, mut args: Value) -> Result<(), RunnerError> {
        if self.journal.pending.is_some() {
            return Err(RunnerError::Journal);
        }
        args["idempotency_key"] = json!(new_id().map_err(|_| RunnerError::Io)?);
        self.journal.pending = Some(PendingMutation::new(
            ToolRequest { tool, args },
            &self.config.catalog,
        )?);
        self.save()
    }
    async fn replay(&mut self, recovering: bool) -> Result<(), RunnerError> {
        let pending = self.journal.pending.clone().ok_or(RunnerError::Journal)?;
        let mut result = None;
        for attempt in 0..3 {
            match self
                .attempt(pending.request.clone(), recovering, false)
                .await
            {
                Ok(Some(value)) => {
                    result = Some(value);
                    break;
                }
                Err(RunnerError::Transport) if attempt < 2 => {
                    self.delay(attempt + 1, recovering).await?
                }
                Err(error) => return Err(error),
                Ok(None) => return Err(RunnerError::Contract),
            }
        }
        let value = result.ok_or(RunnerError::Transport)?;
        let args = &pending.request.args;
        match pending.request.tool {
            AllowedTool::Claim => {
                let claim: ClaimResult = body(value)?;
                if claim.run_id.is_empty() || claim.leases.len() > self.config.batch_size as usize {
                    return Err(RunnerError::Contract);
                }
                self.journal.run_id = Some(claim.run_id);
                self.journal.work = claim
                    .leases
                    .iter()
                    .map(|l| WorkState {
                        work_id: l.work_id.clone(),
                        phase: Phase::Claimed,
                        packet: None,
                        proposal: None,
                        usage: None,
                        failure: None,
                    })
                    .collect();
                for l in &claim.leases {
                    timestamp(&l.expires_at)?;
                    self.renew_at
                        .insert(l.work_id.clone(), Instant::now() + HEARTBEAT);
                }
                self.journal.leases = claim.leases;
            }
            AllowedTool::Renew => {
                let renewed: Lease = body(value)?;
                let old = self
                    .journal
                    .leases
                    .iter_mut()
                    .find(|l| args["work_id"] == l.work_id)
                    .ok_or(RunnerError::Contract)?;
                if renewed.run_id != old.run_id
                    || renewed.work_id != old.work_id
                    || renewed.token != old.token
                    || renewed.pin != old.pin
                    || renewed.reasons != old.reasons
                    || timestamp(&renewed.expires_at)? < timestamp(&old.expires_at)?
                {
                    return Err(RunnerError::LeaseLost);
                }
                self.renew_at
                    .insert(old.work_id.clone(), Instant::now() + HEARTBEAT);
                *old = renewed;
            }
            AllowedTool::Submit => {
                let receipt: Receipt = body(value)?;
                let outcome_matches = if receipt.outcome == WorkOutcome::Superseded {
                    receipt.outdated
                } else {
                    serde_json::to_value(receipt.outcome).map_err(|_| RunnerError::Contract)?
                        == args["submission"]["outcome"]
                };
                if Some(&receipt.run_id) != self.journal.run_id.as_ref()
                    || args["work_id"] != receipt.work_id
                    || !outcome_matches
                {
                    return Err(RunnerError::Contract);
                }
                let work = self
                    .journal
                    .work
                    .iter_mut()
                    .find(|w| w.work_id == receipt.work_id)
                    .ok_or(RunnerError::Contract)?;
                work.phase = Phase::Reported;
                self.renew_at.remove(&receipt.work_id);
                self.journal.receipts.push(receipt);
            }
            AllowedTool::Close => {
                if value["id"] != args["run_id"] || value["state"] != args["state"] {
                    return Err(RunnerError::Contract);
                }
                self.journal.terminal = true;
                for w in &mut self.journal.work {
                    if w.phase != Phase::Reported {
                        if w.phase == Phase::InvocationStarted && w.usage.is_none() {
                            w.failure = Some("unknown_accounting".into());
                        }
                        w.phase = Phase::Abandoned;
                    }
                }
            }
            _ => return Err(RunnerError::Contract),
        }
        self.journal.pending = None;
        self.save()?;
        // Receipt persistence precedes any authority check or progress. Old replayed
        // renewals are acknowledged locally but cannot authorize further work.
        self.check(recovering)
    }
    async fn heartbeat(&mut self, ahead: Duration) -> Result<(), RunnerError> {
        self.check(false)?;
        if self.journal.pending.is_some() {
            self.replay(false).await?;
        }
        let now = Instant::now();
        let scheduled = self.next_renew().is_some_and(|at| at <= now);
        let urgent = self.active_leases().any(|l| {
            self.renew_at
                .get(&l.work_id)
                .is_none_or(|at| *at <= now + ahead)
        });
        if !scheduled && !urgent {
            return Ok(());
        }
        // Snapshot a finite sweep. Its own acknowledgments cannot re-enter it,
        // and the next early sweep is scheduled from this sweep, not each ack.
        let mut leases: Vec<_> = self.active_leases().cloned().collect();
        leases.sort_by_key(|l| self.renew_at.get(&l.work_id).copied());
        for lease in leases {
            self.prepare(
                AllowedTool::Renew,
                json!({"work_id":lease.work_id,"token":lease.token}),
            )?;
            self.replay(false).await?;
        }
        self.sweep_at = Some(now + HEARTBEAT);
        self.check(false)
    }
    async fn execute<I: Inference + 'static>(
        &mut self,
        inference: Arc<I>,
    ) -> Result<(), RunnerError> {
        self.check(true)?;
        let recovering = self.journal.run_id.is_some() || self.journal.pending.is_some();
        let closing_reason = self
            .journal
            .pending
            .as_ref()
            .filter(|p| p.request.tool == AllowedTool::Close)
            .and_then(|p| p.request.args["rationale"].as_str())
            .map(str::to_owned);
        if self.journal.pending.is_some() {
            self.replay(true).await?;
        }
        if self.journal.terminal {
            return self
                .journal
                .stop_reason
                .as_deref()
                .or(closing_reason.as_deref())
                .and_then(failure_kind)
                .map_or(Ok(()), Err);
        }
        if self.journal.run_id.is_none() {
            if let Some(error) = self.journal.stop_reason.as_deref().and_then(failure_kind) {
                return Err(error);
            }
            self.prepare(
                AllowedTool::Claim,
                json!({"batch_size":self.config.batch_size}),
            )?;
            self.replay(true).await?;
        }
        // Deadline lookup follows exact replay, without manufacturing a replacement claim.
        let request = normalized(
            ToolRequest {
                tool: AllowedTool::GetRun,
                args: json!({"run_id":self.journal.run_id}),
            },
            &self.config.catalog,
        )?;
        let mut report = None;
        for attempt in 0..3 {
            match self.attempt(request.clone(), true, false).await {
                Ok(Some(value)) => {
                    report = Some(value);
                    break;
                }
                Err(RunnerError::Transport) if attempt < 2 => self.delay(attempt + 1, true).await?,
                Err(error) => return Err(error),
                _ => return Err(RunnerError::Contract),
            }
        }
        let report = report.ok_or(RunnerError::Transport)?;
        if report["run"]["id"] != json!(self.journal.run_id) {
            return Err(RunnerError::Contract);
        }
        self.deadline = Some(timestamp(
            report["run"]["deadline"]
                .as_str()
                .ok_or(RunnerError::Contract)?,
        )?);
        if !matches!(
            report["run"]["state"].as_str(),
            Some("running" | "completed")
        ) {
            return Err(RunnerError::LeaseLost);
        }
        if report["run"]["state"] == "completed"
            && self.journal.work.iter().any(|w| w.phase != Phase::Reported)
        {
            return Err(RunnerError::LeaseLost);
        }
        if recovering {
            let ids: Vec<_> = self.active_leases().map(|l| l.work_id.clone()).collect();
            for id in ids {
                self.renew_at.insert(id, Instant::now());
            }
        }
        self.heartbeat(Duration::ZERO).await?;
        if let Some(reason) = self.journal.stop_reason.as_deref().and_then(failure_kind) {
            // Replay, run lookup and renewal established current authority. The
            // immutable first stop forbids inference, not acknowledged cleanup.
            return self.finish(Some(reason)).await;
        }
        // Even reconciled usage never authorizes another invocation after interruption.
        if !self.journal.accounting_known
            || self.journal.work.iter().any(|w| {
                w.phase == Phase::InvocationStarted
                    || w.failure.as_deref() == Some("unknown_accounting")
            })
        {
            return Err(RunnerError::UnknownAccounting);
        }
        if let Some(error) = self
            .journal
            .work
            .iter()
            .filter_map(|w| w.failure.as_deref().and_then(failure_kind))
            .next()
        {
            return Err(error);
        }
        self.observed_budget()?;
        for index in 0..self.journal.work.len() {
            self.heartbeat(Duration::ZERO).await?;
            match self.journal.work[index].phase {
                Phase::Reported | Phase::Abandoned => continue,
                Phase::Claimed => {
                    let lease = self
                        .journal
                        .leases
                        .iter()
                        .find(|l| l.work_id == self.journal.work[index].work_id)
                        .ok_or(RunnerError::Journal)?;
                    let mut collector = Collector::new(lease)?;
                    loop {
                        self.heartbeat(Duration::ZERO).await?;
                        match collector.next()? {
                            CollectStep::Request(request) => {
                                let response = self.read(request).await?;
                                collector.accept(response)?;
                            }
                            CollectStep::Ready(packet) => {
                                self.journal.work[index].packet = Some(packet);
                                self.journal.work[index].phase = Phase::PacketReady;
                                self.save()?;
                                break;
                            }
                        }
                    }
                }
                _ => {}
            }
            if self.journal.work[index].phase == Phase::PacketReady {
                self.heartbeat(Duration::ZERO).await?;
                self.budget()?;
                let prompt = render_prompt(
                    self.journal.work[index]
                        .packet
                        .as_ref()
                        .ok_or(RunnerError::Journal)?,
                )?;
                self.check(false)?;
                self.journal.work[index].phase = Phase::InvocationStarted;
                self.journal.invocations = self
                    .journal
                    .invocations
                    .checked_add(1)
                    .ok_or(RunnerError::Budget)?;
                self.journal.reserved_microusd = self
                    .journal
                    .reserved_microusd
                    .checked_add(self.config.invocation_microusd)
                    .ok_or(RunnerError::Budget)?;
                self.journal.accounting_known = false;
                self.save()?;
                let request = ModelRequest {
                    prompt,
                    model: self.config.model.clone(),
                    budget_usd: format!(
                        "{}.{:06}",
                        self.config.invocation_microusd / 1_000_000,
                        self.config.invocation_microusd % 1_000_000
                    ),
                };
                self.check(false)?;
                let token = CancellationToken::new();
                let child_token = token.clone();
                let adapter = inference.clone();
                self.child = Some(Running {
                    index,
                    cancel: token,
                    until: Instant::now() + CHILD,
                    task: tokio::spawn(async move { adapter.infer(request, child_token).await }),
                });
                loop {
                    self.heartbeat(Duration::ZERO).await?;
                    self.check(false)?;
                    let child = self.child.as_mut().ok_or(RunnerError::Journal)?;
                    let output = tokio::select! {
                        biased;
                        _=self.cancel.cancelled()=>return Err(RunnerError::Cancelled),
                        result=&mut child.task=>Some(result),
                        _=tokio::time::sleep(Duration::from_secs(1))=>None,
                    };
                    if let Some(result) = output {
                        self.child = None;
                        self.record_model(
                            index,
                            result.unwrap_or(Err(RunnerError::UnknownAccounting)),
                        )?;
                        break;
                    }
                }
            }
            self.submit(index, None).await?;
            // Overshoot cannot be prevented retroactively, even on the last item.
            self.observed_budget()?;
        }
        self.finish(None).await
    }
    fn observed_budget(&self) -> Result<(), RunnerError> {
        if tokens(&self.journal.usage).is_none_or(|n| n >= self.config.observed_token_stop)
            || self.journal.usage.cost_microusd > self.config.run_microusd
            || self.journal.work.iter().any(|w| {
                w.usage
                    .as_ref()
                    .is_some_and(|u| u.cost_microusd > self.config.invocation_microusd)
            })
        {
            return Err(RunnerError::Budget);
        }
        Ok(())
    }
    fn budget(&self) -> Result<(), RunnerError> {
        self.check(false)?;
        if !self.journal.accounting_known {
            return Err(RunnerError::UnknownAccounting);
        }
        self.observed_budget()?;
        if self.journal.invocations >= self.config.max_invocations
            || self
                .journal
                .reserved_microusd
                .max(self.journal.usage.cost_microusd)
                .checked_add(self.config.invocation_microusd)
                .is_none_or(|n| n > self.config.run_microusd)
        {
            return Err(RunnerError::Budget);
        }
        Ok(())
    }
    fn record_model(
        &mut self,
        index: usize,
        result: Result<ModelResult, RunnerError>,
    ) -> Result<(), RunnerError> {
        let result = result.ok();
        let totals = result
            .as_ref()
            .and_then(|r| add_usage(&self.journal.usage, &r.usage));
        if let (Some(result), Some(totals)) = (result, totals) {
            let invalid = to_submission(
                self.journal.work[index]
                    .packet
                    .as_ref()
                    .ok_or(RunnerError::Journal)?,
                &result.proposal,
            )
            .is_err();
            let work = &mut self.journal.work[index];
            // ProposalReady stores decoded results, including a rejected proposal
            // tagged with failure. Such a proposal is never submitted as findings.
            work.phase = Phase::ProposalReady;
            work.proposal = Some(result.proposal);
            work.usage = Some(result.usage);
            work.failure = invalid.then(|| "invalid_proposal".into());
            self.journal.usage = totals;
            self.journal.accounting_known = true;
            self.save()?;
            if invalid {
                return Err(RunnerError::InvalidProposal);
            }
            self.observed_budget()
        } else {
            self.journal.work[index].failure = Some("unknown_accounting".into());
            self.journal.accounting_known = false;
            self.save()?;
            Err(RunnerError::UnknownAccounting)
        }
    }
    async fn submit(&mut self, index: usize, stop: Option<RunnerError>) -> Result<(), RunnerError> {
        self.heartbeat(MUTATION_WINDOW).await?;
        self.check(false)?;
        let work = &self.journal.work[index];
        if matches!(work.phase, Phase::Reported | Phase::Abandoned) {
            return Ok(());
        }
        let submission = if work.phase == Phase::ProposalReady
            && work.failure.as_deref() != Some("invalid_proposal")
        {
            to_submission(
                work.packet.as_ref().ok_or(RunnerError::Journal)?,
                work.proposal.as_ref().ok_or(RunnerError::Journal)?,
            )?
        } else {
            generic()
        };
        let lease = self
            .journal
            .leases
            .iter()
            .find(|l| l.work_id == work.work_id)
            .ok_or(RunnerError::Journal)?
            .clone();
        let work = &mut self.journal.work[index];
        if work.phase == Phase::InvocationStarted && work.usage.is_none() {
            work.failure = Some("unknown_accounting".into());
        } else if work.failure.is_none() && work.phase != Phase::ProposalReady {
            // unknown_accounting is a journal invocation marker, not a generic
            // reason for queued work which never started. Its failed receipt suffices.
            work.failure = stop
                .filter(|e| *e != RunnerError::UnknownAccounting)
                .map(|e| kind(e).into());
        }
        work.phase = Phase::SubmissionPending;
        self.prepare(
            AllowedTool::Submit,
            json!({"work_id":lease.work_id,"token":lease.token,"submission":submission}),
        )?;
        self.replay(false).await
    }
    async fn finish(&mut self, stop: Option<RunnerError>) -> Result<(), RunnerError> {
        if self.journal.terminal {
            return Ok(());
        }
        self.check(false)?;
        if self.journal.pending.is_some() {
            self.replay(false).await?;
        }
        if self.journal.run_id.is_none() || self.deadline.is_none() {
            return Err(RunnerError::LeaseLost);
        }
        for index in 0..self.journal.work.len() {
            self.submit(index, stop).await?;
        }
        self.check(false)?;
        let state = if self.journal.work.iter().all(|w| w.phase == Phase::Reported) {
            "completed"
        } else {
            "failed"
        };
        self.prepare(AllowedTool::Close,json!({"run_id":self.journal.run_id,"state":state,"rationale":stop.map(kind).unwrap_or("completed")}))?;
        self.replay(false).await
    }
    fn summary(&self, reason: &str) -> RunSummary {
        let count = |outcome| {
            self.journal
                .receipts
                .iter()
                .filter(|r| r.outcome == outcome)
                .count() as u32
        };
        RunSummary {
            run_id: self.journal.run_id.clone(),
            reported: count(WorkOutcome::Completed) + count(WorkOutcome::Superseded),
            inconclusive: count(WorkOutcome::Inconclusive),
            failed: count(WorkOutcome::Failed),
            skipped: (self.journal.work.len() - self.journal.receipts.len()) as u32,
            invocations: self.journal.invocations,
            accounting_known: self.journal.accounting_known,
            reason: self.journal.stop_reason.as_deref().unwrap_or(reason).into(),
        }
    }
}
fn timestamp(value: &str) -> Result<DateTime<Utc>, RunnerError> {
    DateTime::parse_from_rfc3339(value)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|_| RunnerError::Contract)
}
fn normalized(request: ToolRequest, catalog: &str) -> Result<ToolRequest, RunnerError> {
    let tool = request.tool;
    Ok(ToolRequest {
        tool,
        args: Value::Object(contract::arguments(request, catalog)?),
    })
}
fn body<T: DeserializeOwned>(mut value: Value) -> Result<T, RunnerError> {
    value
        .as_object_mut()
        .ok_or(RunnerError::Contract)?
        .remove("catalog");
    serde_json::from_value(value).map_err(|_| RunnerError::Contract)
}
fn tokens(usage: &ModelUsage) -> Option<u64> {
    usage
        .input
        .checked_add(usage.output)?
        .checked_add(usage.cache_creation)?
        .checked_add(usage.cache_read)
}
fn add_usage(a: &ModelUsage, b: &ModelUsage) -> Option<ModelUsage> {
    Some(ModelUsage {
        input: a.input.checked_add(b.input)?,
        output: a.output.checked_add(b.output)?,
        cache_creation: a.cache_creation.checked_add(b.cache_creation)?,
        cache_read: a.cache_read.checked_add(b.cache_read)?,
        cost_microusd: a.cost_microusd.checked_add(b.cost_microusd)?,
    })
}
fn generic() -> Submission {
    Submission {
        outcome: WorkOutcome::Failed,
        assessment: Assessment::NotAssessed,
        summary: "Investigation stopped without a usable assessment".into(),
        evidence: vec![],
        findings: vec![],
        reported_tokens: None,
    }
}
fn persistence(error: RunnerError) -> bool {
    matches!(error, RunnerError::Journal | RunnerError::Io)
}
fn failure_kind(value: &str) -> Option<RunnerError> {
    [
        RunnerError::Config,
        RunnerError::Permission,
        RunnerError::Contract,
        RunnerError::Transport,
        RunnerError::LeaseLost,
        RunnerError::Budget,
        RunnerError::InvalidEvidence,
        RunnerError::InvalidProposal,
        RunnerError::UnknownAccounting,
        RunnerError::Child,
        RunnerError::Journal,
        RunnerError::Cancelled,
        RunnerError::Io,
    ]
    .into_iter()
    .find(|error| kind(*error) == value)
}
fn kind(error: RunnerError) -> &'static str {
    match error {
        RunnerError::Config => "config",
        RunnerError::Busy => "journal",
        RunnerError::Permission => "permission",
        RunnerError::Contract => "contract",
        RunnerError::Transport => "transport",
        RunnerError::LeaseLost => "lease_lost",
        RunnerError::Budget => "budget",
        RunnerError::InvalidEvidence => "invalid_evidence",
        RunnerError::InvalidProposal => "invalid_proposal",
        RunnerError::UnknownAccounting => "unknown_accounting",
        RunnerError::Child => "child",
        RunnerError::Journal => "journal",
        RunnerError::Cancelled => "cancelled",
        RunnerError::Io => "io",
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "supervisor_tests.rs"]
mod tests;
