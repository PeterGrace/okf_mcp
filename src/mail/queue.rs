//! The `SendMail` job on apalis-postgres (spec "Background jobs";
//! Decision 11's durable retry).
//!
//! The life of a job:
//! 1. [`enqueue`] renders the template and inserts one `apalis.jobs` row in
//!    queue [`QUEUE`] with `max_attempts` [`MAX_ATTEMPTS`]. The payload is
//!    the rendered [`Mail`], the template's name and `queued_at`.
//! 2. A worker in each `serve` process ([`spawn_worker`]) fetches it. apalis
//!    takes jobs with `FOR UPDATE SKIP LOCKED` and marks them as its own, so
//!    two workers (two replicas) never take the same job.
//! 3. The handler sends once. Success: `Done`. A permanent error: an
//!    `AbortError`, so apalis marks the job `Killed` and never fetches it
//!    again. A retryable error within [`RETRY_WINDOW_HOURS`] of `queued_at`:
//!    the handler moves `run_at` to now plus the backoff and returns a plain
//!    error, so apalis marks it `Failed` with one more attempt and fetches it
//!    again once `run_at` passes. After the window: an `AbortError`.
//! 4. [`list`], [`retry`], [`delete`] and [`prune`] read and write
//!    `apalis.jobs` in SQL, where apalis-postgres has no API.

use std::sync::Arc;
use std::time::Duration;

use apalis::prelude::{
    AbortError, BackendExt, BoxDynError, Data, TaskBuilder, TaskId, WorkerBuilder,
};
use apalis_postgres::queries::push_tasks;
use apalis_postgres::{Config as QueueConfig, PostgresStorage};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgPoolOptions;
use sqlx::{PgConnection, PgPool};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::{render, Mail, MailError, Mailer, Template};
use crate::app::App;
use crate::clock::Clock;
use crate::error::OkfError;
use crate::store::users::check_email;

/// The apalis queue (`apalis.jobs.job_type`). Set explicitly: apalis's
/// default is the Rust type's path, which a rename would orphan.
pub const QUEUE: &str = "send_mail";

/// `max_attempts` for every job: more than the 24-hour window can use, so
/// the window, not the count, ends the retries (Decision 11).
pub const MAX_ATTEMPTS: usize = 64;

/// How long a mail keeps being retried after it was queued.
pub const RETRY_WINDOW_HOURS: i64 = 24;

/// The first retry's delay; each later one doubles, up to [`BACKOFF_MAX_SECS`].
const BACKOFF_FIRST_SECS: f64 = 30.0;

/// The longest delay between two tries.
const BACKOFF_MAX_SECS: f64 = 3600.0;

/// How often an idle worker looks for due jobs that no NOTIFY announced
/// (rescheduled and manually retried ones).
pub const POLL_INTERVAL: Duration = Duration::from_secs(30);

/// How long a worker that stopped on an error waits before starting again.
const RESTART_DELAY: Duration = Duration::from_secs(30);

/// How often a worker tells apalis it is alive (apalis's default).
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

/// The connections the worker's own pool may open: one held by LISTEN, one
/// for the fetch, ack and heartbeat cycle, and two for handlers
/// rescheduling at the same time. Never taken from the request pool, so
/// `database_max_connections` stays the request side's alone.
pub const WORKER_POOL_SIZE: u32 = 4;

/// The `application_name` the worker's connections carry, so an operator
/// (or a test) can tell them apart in `pg_stat_activity`.
pub const WORKER_APPLICATION_NAME: &str = "okf-mail-worker";

/// How long sent mail jobs are kept.
const DONE_RETENTION_DAYS: i64 = 7;

/// How long failed mail jobs are kept.
const FAILED_RETENTION_DAYS: i64 = 30;

/// The job's payload: the rendered mail, its template's name (for the
/// queue listing) and when it was queued (Unix seconds, server clock; the
/// start of its retry window). `Debug` omits the mail's bodies.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct SendMail {
    mail: Mail,
    template: String,
    queued_at: i64,
}

impl std::fmt::Debug for SendMail {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `Mail`'s own `Debug` already leaves the bodies out.
        formatter
            .debug_struct("SendMail")
            .field("mail", &self.mail)
            .field("template", &self.template)
            .field("queued_at", &self.queued_at)
            .finish()
    }
}

/// One mail job as `mail queue` and the Settings page show it: never the body.
// `sqlx::FromRow` maps a result row's columns to the fields by name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct QueuedMail {
    /// The job id (a ULID), for retry and delete.
    pub id: String,
    /// The recipient.
    pub to: String,
    /// The template's name.
    pub template: String,
    /// `pending`, `sending`, `retrying` or `failed`.
    pub status: String,
    /// Tries so far.
    pub attempts: i32,
    /// The last try's error, if it failed.
    pub last_error: Option<String>,
    /// When it is (or was) due.
    pub run_at: DateTime<Utc>,
}

/// The error for a queue operation apalis refused (its own error type).
fn queue_error(error: impl std::fmt::Display) -> OkfError {
    OkfError::Io {
        path: "<mail queue>".to_string(),
        source: std::io::Error::other(error.to_string()),
    }
}

/// Renders a template and queues it for `to`. Call it after the request's
/// own transaction commits, so a rolled-back change sends nothing, or use
/// [`enqueue_in`] inside that transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `to` - one recipient address.
/// * `template` - what to send.
/// * `now` - when (server clock): the start of the 24-hour retry window.
///
/// # Errors
/// As [`enqueue_in`].
pub async fn enqueue(
    pool: &PgPool,
    to: &str,
    template: &Template,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut conn = pool.acquire().await?;
    // `&mut *conn`: the pooled connection's inner `PgConnection`.
    enqueue_in(&mut conn, to, template, now).await
}

/// [`enqueue`], inside the caller's transaction: the job exists only if the
/// change it reports commits (apalis' notify is delivered at the commit),
/// and nothing runs after that commit (a sign-in's "method added" notice).
///
/// # Arguments
/// * `conn` - the caller's connection or transaction (`&mut tx`).
/// * `to` - one recipient address.
/// * `template` - what to send.
/// * `now` - when (server clock): the start of the 24-hour retry window.
///
/// # Errors
/// [`OkfError::LimitExceeded`] with `limit: "email"` for a malformed
/// address; [`OkfError::Database`] or [`OkfError::Io`] if the insert fails.
pub async fn enqueue_in(
    conn: &mut PgConnection,
    to: &str,
    template: &Template,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    check_email(to)?;
    let (subject, text, html) = render(template);
    let job = SendMail {
        mail: Mail {
            to: to.to_string(),
            subject,
            text,
            html,
        },
        template: template.name().to_string(),
        queued_at: now.timestamp(),
    };
    // apalis-postgres stores the payload as JSON bytes (its JsonCodec), so
    // encode it the same way the worker decodes it.
    let payload = serde_json::to_vec(&job).map_err(queue_error)?;
    let task = TaskBuilder::new(payload).max_attempts(MAX_ATTEMPTS).build();
    push_tasks(&mut *conn, QUEUE, vec![task])
        .await
        .map_err(queue_error)?;
    tracing::info!(template = template.name(), "mail queued");
    Ok(())
}

/// Every mail job that is not done, oldest due first.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The jobs (pending, sending, retrying and failed), without their bodies.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<QueuedMail>, OkfError> {
    // `job` is the JSON payload as bytes; `convert_from(..)::jsonb` reads
    // it, and only the recipient and the template's name leave the
    // database. `"to"` is quoted: TO is an SQL keyword.
    Ok(sqlx::query_as::<_, QueuedMail>(
        "SELECT id, \
                convert_from(job, 'UTF8')::jsonb -> 'mail' ->> 'to' AS \"to\", \
                convert_from(job, 'UTF8')::jsonb ->> 'template' AS template, \
                CASE WHEN status = 'Killed' \
                       OR (status = 'Failed' AND attempts >= max_attempts) THEN 'failed' \
                     WHEN status = 'Failed' THEN 'retrying' \
                     WHEN status IN ('Queued', 'Running') THEN 'sending' \
                     ELSE 'pending' END AS status, \
                attempts, last_result ->> 'Err' AS last_error, run_at \
         FROM apalis.jobs WHERE job_type = $1 AND status <> 'Done' \
         ORDER BY run_at, id",
    )
    .bind(QUEUE)
    .fetch_all(pool)
    .await?)
}

/// Whether `id` can be a job id (a ULID: ASCII letters and digits). Text
/// that cannot (a NUL, which Postgres refuses as a database error, among
/// others) names no job and never reaches SQL.
fn is_job_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

/// The error for a job id that names no mail job.
fn unknown_job(id: &str) -> OkfError {
    OkfError::UnknownName {
        noun: "mail job",
        name: id.to_string(),
    }
}

/// Explains why a job-id write matched no row: the job is unknown, or in a
/// state the operation does not take.
async fn refusal(pool: &PgPool, id: &str, why: &str) -> Result<OkfError, OkfError> {
    let exists: Option<String> =
        sqlx::query_scalar("SELECT status FROM apalis.jobs WHERE id = $1 AND job_type = $2")
            .bind(id)
            .bind(QUEUE)
            .fetch_optional(pool)
            .await?;
    Ok(match exists {
        None => unknown_job(id),
        Some(_) => OkfError::Refused {
            reason: format!("mail job {id} {why}"),
        },
    })
}

/// Puts a failed (or retrying) job back in the queue: due now, no attempts,
/// and a fresh 24-hour window from `now`.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the job id from [`list`].
/// * `now` - when (server clock).
///
/// # Errors
/// [`OkfError::UnknownName`] (`noun: "mail job"`); [`OkfError::Refused`]
/// for a job that has not failed; [`OkfError::Database`].
pub async fn retry(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    if !is_job_id(id) {
        return Err(unknown_job(id));
    }
    // `jsonb_set` rewrites `queued_at` inside the stored payload. No NOTIFY
    // fires for an UPDATE; the worker's interval poll finds the job. A row
    // a worker holds is no longer `Failed`, so it is never taken back.
    let updated = sqlx::query(
        "UPDATE apalis.jobs SET status = 'Pending', attempts = 0, run_at = now(), \
             lock_by = NULL, lock_at = NULL, done_at = NULL, last_result = NULL, \
             job = convert_to(jsonb_set(convert_from(job, 'UTF8')::jsonb, \
                                        '{queued_at}', to_jsonb($3::bigint))::text, 'UTF8') \
         WHERE id = $1 AND job_type = $2 AND status IN ('Failed', 'Killed')",
    )
    .bind(id)
    .bind(QUEUE)
    .bind(now.timestamp())
    .execute(pool)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(refusal(pool, id, "has not failed; only failed mail is retried").await?);
    }
    tracing::info!(job = id, "mail job queued again");
    Ok(())
}

/// Deletes a job that is not being sent right now.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the job id from [`list`].
///
/// # Errors
/// [`OkfError::UnknownName`] (`noun: "mail job"`); [`OkfError::Refused`]
/// while a worker holds it; [`OkfError::Database`].
pub async fn delete(pool: &PgPool, id: &str) -> Result<(), OkfError> {
    if !is_job_id(id) {
        return Err(unknown_job(id));
    }
    let deleted = sqlx::query(
        "DELETE FROM apalis.jobs \
         WHERE id = $1 AND job_type = $2 AND status NOT IN ('Queued', 'Running')",
    )
    .bind(id)
    .bind(QUEUE)
    .execute(pool)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(refusal(pool, id, "is being sent; try again in a moment").await?);
    }
    tracing::info!(job = id, "mail job deleted");
    Ok(())
}

/// Deletes sent jobs finished over 7 days before `now`, and failed ones
/// over 30 (spec "Secrets in payloads"). Called by `App::poll`'s hourly prune.
///
/// # Arguments
/// * `pool` - the database.
/// * `now` - the server clock.
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(sqlx::query(
        "DELETE FROM apalis.jobs WHERE job_type = $1 AND ( \
             (status = 'Done' AND done_at < $2) \
          OR ((status = 'Killed' OR (status = 'Failed' AND attempts >= max_attempts)) \
              AND done_at < $3))",
    )
    .bind(QUEUE)
    .bind(now - TimeDelta::days(DONE_RETENTION_DAYS))
    .bind(now - TimeDelta::days(FAILED_RETENTION_DAYS))
    .execute(pool)
    .await?
    .rows_affected())
}

/// What the job handler needs, given to apalis as worker data (cloned per
/// job: every field is a cheap handle).
#[derive(Clone)]
pub(crate) struct MailContext {
    /// The worker's own pool ([`worker_pool`]): apalis's LISTEN, fetches,
    /// acks and heartbeats, and the handler's reschedules.
    pub pool: PgPool,
    /// The provider.
    pub mailer: Arc<dyn Mailer>,
    /// The server clock (for the retry window).
    pub clock: Arc<dyn Clock>,
}

/// Moves a job's `run_at` to now plus its backoff: 30 s doubling per
/// attempt so far, capped at an hour. In SQL, on the database's clock,
/// which is the one apalis compares `run_at` with.
async fn reschedule(pool: &PgPool, id: &str) -> Result<(), OkfError> {
    // `attempts` is the count before this try is acked (0 on the first
    // failure), so the first retry waits 30 s. `LEAST(attempts, 16)` keeps
    // `power` small; the hour cap is reached at 7 already.
    sqlx::query(
        "UPDATE apalis.jobs SET run_at = now() + make_interval(secs => \
             LEAST($3, $4 * power(2.0::float8, LEAST(attempts, 16)))) \
         WHERE id = $1 AND job_type = $2",
    )
    .bind(id)
    .bind(QUEUE)
    .bind(BACKOFF_MAX_SECS)
    .bind(BACKOFF_FIRST_SECS)
    .execute(pool)
    .await?;
    Ok(())
}

/// The job handler: one send, then Decision 11's outcome (see the module
/// doc). apalis fills each argument from the job: the payload, the worker
/// data and the job id.
async fn send_mail(
    job: SendMail,
    context: Data<MailContext>,
    id: TaskId,
) -> Result<(), BoxDynError> {
    let id = id.to_string();
    let error = match context.mailer.send(&job.mail).await {
        Ok(()) => {
            tracing::info!(job = %id, template = %job.template, "mail sent");
            return Ok(());
        }
        Err(error) => error,
    };
    // `Box::new(...)` makes the boxed error trait object apalis expects;
    // `AbortError` tells it never to fetch the job again.
    match error {
        MailError::Permanent(_) => {
            tracing::warn!(job = %id, template = %job.template, %error, "mail refused for good");
            Err(Box::new(AbortError::new(error)))
        }
        MailError::Retryable(reason) => {
            let age = context.clock.now().timestamp() - job.queued_at;
            if age >= RETRY_WINDOW_HOURS * 60 * 60 {
                tracing::warn!(job = %id, template = %job.template, %reason, "mail given up");
                return Err(Box::new(AbortError::new(MailError::Retryable(format!(
                    "gave up after {RETRY_WINDOW_HOURS} hours: {reason}"
                )))));
            }
            if let Err(error) = reschedule(&context.pool, &id).await {
                // Still a plain error: apalis fetches it again, just sooner.
                tracing::error!(job = %id, %error, "rescheduling a mail job failed");
            }
            tracing::info!(job = %id, template = %job.template, %reason, "mail failed; will retry");
            Err(Box::new(MailError::Retryable(reason)))
        }
    }
}

/// This process's worker name: unique per host and process, so replicas
/// never share an `apalis.workers` row (Decision 11).
///
/// # Returns
/// `okf-mail-<host>-<pid>`.
fn worker_name() -> String {
    // `$HOSTNAME`, else the kernel's name for the host, else a placeholder.
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok())
        .map(|host| host.trim().to_string())
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "host".to_string());
    format!("okf-mail-{host}-{}", std::process::id())
}

/// The worker's own pool: the same database and credentials as `pool`, at
/// most [`WORKER_POOL_SIZE`] connections, named [`WORKER_APPLICATION_NAME`].
/// It connects lazily, on the worker's first query.
///
/// # Arguments
/// * `pool` - the request pool, whose connect options are copied.
///
/// # Returns
/// A new pool; closing it leaves `pool` untouched.
pub(crate) fn worker_pool(pool: &PgPool) -> PgPool {
    // `connect_options()` is shared (`Arc`); `as_ref().clone()` copies it so
    // the name can be changed for this pool only.
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .application_name(WORKER_APPLICATION_NAME);
    PgPoolOptions::new()
        .max_connections(WORKER_POOL_SIZE)
        .connect_lazy_with(options)
}

/// How often a worker polls, heartbeats and restarts. Tests shorten them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WorkerTiming {
    /// How often to look for due jobs no NOTIFY announced.
    pub poll: Duration,
    /// How often to heartbeat (and re-enqueue jobs of dead workers).
    pub heartbeat: Duration,
    /// How long to wait before restarting after a backend error.
    pub restart: Duration,
}

impl WorkerTiming {
    /// The timing `serve` uses.
    pub(crate) const SERVE: Self = Self {
        poll: POLL_INTERVAL,
        heartbeat: HEARTBEAT_INTERVAL,
        restart: RESTART_DELAY,
    };
}

/// Starts the mail worker for a serving process (`http::serve` calls it,
/// unless `[mail] worker = false`).
///
/// # Arguments
/// * `app` - the shared state (its pool's connect options, mailer and clock).
/// * `shutdown` - cancelled when the server stops.
///
/// # Returns
/// The worker's task; it ends once `shutdown` is cancelled and the job in
/// hand (if any) is finished.
pub fn spawn_worker(app: Arc<App>, shutdown: CancellationToken) -> JoinHandle<()> {
    let context = MailContext {
        pool: worker_pool(app.pool()),
        mailer: Arc::clone(app.mailer()),
        clock: app.clock_handle(),
    };
    run_worker(context, worker_name(), WorkerTiming::SERVE, shutdown)
}

/// Runs one apalis worker on [`QUEUE`] until `shutdown`, restarting it
/// after `timing.restart` if it stops on an error (a lost database, say).
/// It closes `context.pool` when it ends.
///
/// # Arguments
/// * `context` - what the handler needs; its pool is the worker's own.
/// * `name` - the worker's name ([`worker_name`]); a restart registers as
///   `<name>-<restarts>`, a row of its own, so apalis's session-scoped lock
///   on the old row can never refuse it.
/// * `timing` - the poll, heartbeat and restart delays.
/// * `shutdown` - cancelled to stop.
///
/// # Returns
/// The worker's task.
pub(crate) fn run_worker(
    context: MailContext,
    name: String,
    timing: WorkerTiming,
    shutdown: CancellationToken,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        run_until_cancelled(&context, &name, timing, &shutdown).await;
        context.pool.close().await;
    })
}

/// [`run_worker`]'s loop: returns once `shutdown` is cancelled.
async fn run_until_cancelled(
    context: &MailContext,
    name: &str,
    timing: WorkerTiming,
    shutdown: &CancellationToken,
) {
    // `0..`: an endless range, counting the restarts.
    for restarts in 0_u64.. {
        let config = QueueConfig::default()
            .queue(QUEUE)
            .heartbeat_interval(timing.heartbeat);
        let backend = PostgresStorage::<SendMail>::new(&context.pool)
            .with_config(config)
            .with_pubsub()
            .poll_with_interval(timing.poll);
        let registered = if restarts == 0 {
            name.to_string()
        } else {
            format!("{name}-{restarts}")
        };
        let worker = WorkerBuilder::new(registered)
            .backend(backend)
            .data(context.clone())
            .build(send_mail);
        let stop = shutdown.clone();
        // The future apalis waits on: it resolves when we are cancelled.
        let signal = async move {
            stop.cancelled().await;
            Ok::<(), std::io::Error>(())
        };
        match worker.run_until(signal).await {
            Ok(()) => return,
            Err(error) => {
                tracing::error!(%error, "the mail worker stopped; restarting it soon");
            }
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(timing.restart) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use crate::clock::FixedClock;
    use crate::db::test_db;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A mailer that answers from a script (then `Ok` once it runs out) and
    /// remembers what it was asked to send.
    #[derive(Default)]
    struct Scripted {
        answers: Mutex<VecDeque<Result<(), MailError>>>,
        sent: Mutex<Vec<Mail>>,
        delay: Duration,
    }

    impl Scripted {
        fn with(answers: Vec<Result<(), MailError>>) -> Arc<Self> {
            Arc::new(Self {
                answers: Mutex::new(answers.into()),
                ..Self::default()
            })
        }

        fn sent(&self) -> usize {
            self.sent.lock().unwrap().len()
        }
    }

    #[async_trait::async_trait]
    impl Mailer for Scripted {
        async fn send(&self, mail: &Mail) -> Result<(), MailError> {
            tokio::time::sleep(self.delay).await;
            self.sent.lock().unwrap().push(mail.clone());
            self.answers.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
    }

    /// Fast timing for tests: polls every 100 ms, heartbeats every second
    /// and restarts after 200 ms.
    const FAST: WorkerTiming = WorkerTiming {
        poll: Duration::from_millis(100),
        heartbeat: Duration::from_secs(1),
        restart: Duration::from_millis(200),
    };

    /// Runs one worker named `name` on `own_pool` (a [`worker_pool`], as
    /// `serve`'s is), with [`FAST`] timing, until the returned token is
    /// cancelled. The test keeps a clone of the pool to [`starve`] it.
    fn start_on(
        own_pool: PgPool,
        mailer: Arc<Scripted>,
        name: &str,
    ) -> (CancellationToken, JoinHandle<()>) {
        let context = MailContext {
            pool: own_pool,
            mailer,
            clock: Arc::new(FixedClock(now())),
        };
        let shutdown = CancellationToken::new();
        let worker = run_worker(context, name.to_string(), FAST, shutdown.clone());
        (shutdown, worker)
    }

    fn start(pool: &PgPool, mailer: Arc<Scripted>) -> (CancellationToken, JoinHandle<()>) {
        start_on(worker_pool(pool), mailer, &worker_name())
    }

    /// Holds every connection of a worker's pool that its LISTEN does not
    /// (the listener keeps one for good), so the worker can neither claim
    /// nor finish a job until the returned connections are dropped. Its
    /// LISTEN still hears every insert.
    async fn starve(own_pool: &PgPool) -> Vec<sqlx::pool::PoolConnection<sqlx::Postgres>> {
        let mut held = Vec::with_capacity(WORKER_POOL_SIZE as usize - 1);
        // `1..WORKER_POOL_SIZE`: one fewer than the pool's size, the LISTEN's.
        for _ in 1..WORKER_POOL_SIZE {
            let connection = tokio::time::timeout(Duration::from_secs(10), own_pool.acquire())
                .await
                .expect("a worker connection frees up within 10 s")
                .unwrap();
            held.push(connection);
        }
        held
    }

    /// Waits (up to 10 s) until `count` worker sessions on this test's
    /// database are LISTENing.
    async fn wait_listening(pool: &PgPool, count: i64) {
        let mut listening = 0_i64;
        for _ in 0..100 {
            listening = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND application_name = $1 \
                 AND query LIKE 'LISTEN %'",
            )
            .bind(WORKER_APPLICATION_NAME)
            .fetch_one(pool)
            .await
            .unwrap();
            if listening == count {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("{listening} workers are listening, not {count}");
    }

    /// Enqueues mail to `to` and waits (up to 10 s) for `worker` to claim it.
    async fn enqueue_claimed_by(pool: &PgPool, to: &str, worker: &str) {
        enqueue(pool, to, &Template::Test, now()).await.unwrap();
        let mut claimed_by: Option<String> = None;
        for _ in 0..100 {
            // `convert_from(job, 'UTF8')`: the payload is JSON bytes.
            claimed_by = sqlx::query_scalar(
                "SELECT lock_by FROM apalis.jobs \
                 WHERE convert_from(job, 'UTF8')::jsonb #>> '{mail,to}' = $1",
            )
            .bind(to)
            .fetch_one(pool)
            .await
            .unwrap();
            if claimed_by.as_deref() == Some(worker) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("mail to {to} was claimed by {claimed_by:?}, not {worker}");
    }

    /// Stops a worker and waits for it.
    async fn stop(shutdown: CancellationToken, worker: JoinHandle<()>) {
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(20), worker)
            .await
            .expect("the worker stops within 20 s")
            .unwrap();
    }

    /// The one job's `(status, attempts)`.
    async fn state(pool: &PgPool) -> (String, i32) {
        sqlx::query_as("SELECT status, attempts FROM apalis.jobs WHERE job_type = $1")
            .bind(QUEUE)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Waits (up to 20 s) for the one job to reach `status` with `attempts`.
    async fn wait_for(pool: &PgPool, status: &str, attempts: i32) {
        for _ in 0..200 {
            if state(pool).await == (status.to_string(), attempts) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "the job is {:?}, not ({status}, {attempts})",
            state(pool).await
        );
    }

    /// Seconds from the database's now to the one job's `run_at`.
    async fn wait_left(pool: &PgPool) -> f64 {
        sqlx::query_scalar("SELECT extract(epoch FROM run_at - now())::float8 FROM apalis.jobs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Makes the one job due now, as if its backoff had passed.
    async fn make_due(pool: &PgPool) {
        sqlx::query(
            "UPDATE apalis.jobs SET run_at = now() - interval '1 second' WHERE job_type = $1",
        )
        .bind(QUEUE)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn only_id(pool: &PgPool) -> String {
        sqlx::query_scalar("SELECT id FROM apalis.jobs WHERE job_type = $1")
            .bind(QUEUE)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_enqueued_mail_is_listed_without_its_body() {
        let db = test_db().await;
        let template = Template::Reset {
            link: crate::mail::link("https://okf.example.com", crate::mail::RESET_PATH, "s3cr3t"),
        };

        enqueue(&db.pool, "alice@example.com", &template, now())
            .await
            .unwrap();

        let queued = list(&db.pool).await.unwrap();
        assert_eq!(queued.len(), 1);
        let job = &queued[0];
        assert_eq!(
            (
                job.to.as_str(),
                job.template.as_str(),
                job.status.as_str(),
                job.attempts
            ),
            ("alice@example.com", "reset", "pending", 0)
        );
        assert_eq!(job.last_error, None);
        assert!(!format!("{queued:?}").contains("s3cr3t"));
        let (max, queued_at): (i32, i64) = sqlx::query_as(
            "SELECT max_attempts, (convert_from(job, 'UTF8')::jsonb ->> 'queued_at')::bigint \
             FROM apalis.jobs",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!((max, queued_at), (64, now().timestamp()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_recipient_is_refused_before_it_is_queued() {
        let db = test_db().await;

        let refused = enqueue(
            &db.pool,
            "alice@example.com, bob@example.com",
            &Template::Test,
            now(),
        )
        .await;

        assert!(matches!(
            refused,
            Err(OkfError::LimitExceeded { limit: "email", .. })
        ));
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_worker_sends_a_queued_mail_once() {
        let db = test_db().await;
        let mailer = Scripted::with(vec![]);
        let (shutdown, worker) = start(&db.pool, Arc::clone(&mailer));
        let link = crate::mail::link("https://okf.example.com", crate::mail::SETUP_PATH, "tok");

        let template = Template::Setup {
            link: link.clone(),
            user: "alice".to_string(),
        };
        enqueue(&db.pool, "alice@example.com", &template, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Done", 1).await;
        stop(shutdown, worker).await;
        let listed = list(&db.pool).await.unwrap();

        let sent = mailer.sent.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].to, "alice@example.com");
        assert_eq!(sent[0].subject, "Set up your okf account");
        assert!(sent[0].text.contains(&link), "the link survives the queue");
        assert!(sent[0].html.contains(&link));
        assert!(listed.is_empty(), "done jobs are not listed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provider_error_reschedules_with_doubling_backoff_then_the_retry_succeeds() {
        let db = test_db().await;
        let mailer = Scripted::with(vec![
            Err(MailError::Retryable("connection reset".to_string())),
            Err(MailError::Retryable("timed out".to_string())),
        ]);
        let (shutdown, worker) = start(&db.pool, Arc::clone(&mailer));

        enqueue(&db.pool, "alice@example.com", &Template::Test, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Failed", 1).await;
        let first_wait = wait_left(&db.pool).await;
        let listed = list(&db.pool).await.unwrap();
        // Several polls pass; the job is not due, so nobody takes it.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let sent_while_waiting = mailer.sent();
        let state_while_waiting = state(&db.pool).await;
        make_due(&db.pool).await;
        wait_for(&db.pool, "Failed", 2).await;
        let second_wait = wait_left(&db.pool).await;
        let listed_again = list(&db.pool).await.unwrap();
        make_due(&db.pool).await;
        wait_for(&db.pool, "Done", 3).await;
        stop(shutdown, worker).await;

        assert!(
            (20.0..=30.0).contains(&first_wait),
            "the first retry waits 30 s: {first_wait}"
        );
        assert!(
            (50.0..=60.0).contains(&second_wait),
            "the second waits 60 s: {second_wait}"
        );
        assert_eq!(
            (sent_while_waiting, state_while_waiting),
            (1, ("Failed".to_string(), 1))
        );
        assert_eq!(listed[0].status, "retrying");
        assert!(
            listed[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("connection reset")),
            "{listed:?}"
        );
        assert_eq!(listed_again[0].attempts, 2);
        assert!(
            listed_again[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("timed out")),
            "{listed_again:?}"
        );
        assert_eq!(mailer.sent(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_permanent_error_fails_at_once_and_retry_from_the_queue_sends_it() {
        let db = test_db().await;
        let mailer = Scripted::with(vec![Err(MailError::Permanent(
            "422 invalid To".to_string(),
        ))]);
        let (shutdown, worker) = start(&db.pool, Arc::clone(&mailer));

        enqueue(&db.pool, "alice@example.com", &Template::Test, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Killed", 1).await;
        let listed = list(&db.pool).await.unwrap();
        // Killed is final: polls pass and nobody takes it again.
        tokio::time::sleep(Duration::from_millis(600)).await;
        let sent_before_retry = mailer.sent();
        retry(&db.pool, &only_id(&db.pool).await, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Done", 1).await;
        stop(shutdown, worker).await;

        assert_eq!(listed[0].status, "failed");
        assert!(listed[0]
            .last_error
            .as_deref()
            .is_some_and(|e| e.contains("422 invalid To")));
        assert_eq!(sent_before_retry, 1);
        assert_eq!(mailer.sent(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_mail_still_failing_after_24_hours_gives_up_and_retry_gives_a_new_window() {
        let db = test_db().await;
        let mailer = Scripted::with(vec![
            Err(MailError::Retryable("timed out".to_string())),
            Err(MailError::Retryable("timed out again".to_string())),
        ]);
        let (shutdown, worker) = start(&db.pool, Arc::clone(&mailer));

        // Queued 25 hours before the worker's clock.
        enqueue(
            &db.pool,
            "alice@example.com",
            &Template::Test,
            now() - TimeDelta::hours(25),
        )
        .await
        .unwrap();
        wait_for(&db.pool, "Killed", 1).await;
        let given_up = list(&db.pool).await.unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;
        let sent_after_giving_up = mailer.sent();
        retry(&db.pool, &only_id(&db.pool).await, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Failed", 1).await;
        let retried = list(&db.pool).await.unwrap();
        stop(shutdown, worker).await;

        assert_eq!(given_up[0].status, "failed");
        assert!(
            given_up[0]
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("24 hours") && e.contains("timed out")),
            "{given_up:?}"
        );
        assert_eq!(sent_after_giving_up, 1, "no second try");
        assert_eq!(retried[0].status, "retrying", "a fresh window: {retried:?}");
        assert_eq!(mailer.sent(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_workers_never_send_the_same_mail_twice() {
        let db = test_db().await;
        let mailer = Arc::new(Scripted {
            delay: Duration::from_millis(20),
            ..Scripted::default()
        });
        let recipients: Vec<String> = (0..30).map(|n| format!("user{n}@example.com")).collect();
        // Two replicas: one queue, two worker names, both listening before
        // the mail arrives, so every NOTIFY wakes both.
        let pool_a = worker_pool(&db.pool);
        let pool_b = worker_pool(&db.pool);
        let (stop_a, worker_a) = start_on(pool_a.clone(), Arc::clone(&mailer), "okf-mail-a-1");
        let (stop_b, worker_b) = start_on(pool_b.clone(), Arc::clone(&mailer), "okf-mail-b-2");
        wait_listening(&db.pool, 2).await;
        // apalis claims up to ten jobs a fetch, so either worker can win
        // every race (issue #335). Give each one a mail of its own first,
        // by starving the other of connections while that mail is claimed.
        // `drop` returns the held connections to the pool at once, as
        // leaving a `with` block would in Python.
        let held_b = starve(&pool_b).await;
        enqueue_claimed_by(&db.pool, &recipients[0], "okf-mail-a-1").await;
        drop(held_b);
        let held_a = starve(&pool_a).await;
        enqueue_claimed_by(&db.pool, &recipients[1], "okf-mail-b-2").await;
        drop(held_a);
        // The rest arrive with both workers free to race for each one.
        for to in &recipients[2..] {
            enqueue(&db.pool, to, &Template::Test, now()).await.unwrap();
        }
        let mut done = 0_i64;
        for _ in 0..200 {
            done = sqlx::query_scalar("SELECT count(*) FROM apalis.jobs WHERE status = 'Done'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            if done == 30 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        stop(stop_a, worker_a).await;
        stop(stop_b, worker_b).await;

        assert_eq!(done, 30);
        let mut sent: Vec<String> = mailer
            .sent
            .lock()
            .unwrap()
            .iter()
            .map(|mail| mail.to.clone())
            .collect();
        sent.sort();
        let mut expected = recipients.clone();
        expected.sort();
        assert_eq!(sent, expected, "each mail exactly once");
        let by_worker: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT lock_by FROM apalis.jobs WHERE lock_by IS NOT NULL ORDER BY 1",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            by_worker,
            ["okf-mail-a-1", "okf-mail-b-2"],
            "both workers took mail"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn shutdown_finishes_the_mail_in_hand() {
        let db = test_db().await;
        let mailer = Arc::new(Scripted {
            delay: Duration::from_millis(1500),
            ..Scripted::default()
        });
        let (shutdown, worker) = start(&db.pool, Arc::clone(&mailer));
        enqueue(&db.pool, "alice@example.com", &Template::Test, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Running", 0).await;

        stop(shutdown, worker).await;

        assert_eq!(mailer.sent(), 1);
        assert_eq!(state(&db.pool).await, ("Done".to_string(), 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn retry_and_delete_refuse_unknown_and_unfinished_jobs() {
        let db = test_db().await;
        enqueue(&db.pool, "alice@example.com", &Template::Test, now())
            .await
            .unwrap();
        let id = only_id(&db.pool).await;

        let pending_retry = retry(&db.pool, &id, now()).await;
        let unknown_retry = retry(&db.pool, "no-such-job", now()).await;
        let unknown_delete = delete(&db.pool, "no-such-job").await;
        sqlx::query("UPDATE apalis.jobs SET status = 'Running'")
            .execute(&db.pool)
            .await
            .unwrap();
        let running_delete = delete(&db.pool, &id).await;
        sqlx::query("UPDATE apalis.jobs SET status = 'Pending'")
            .execute(&db.pool)
            .await
            .unwrap();
        delete(&db.pool, &id).await.unwrap();

        assert!(
            matches!(pending_retry, Err(OkfError::Refused { .. })),
            "{pending_retry:?}"
        );
        assert!(matches!(
            unknown_retry,
            Err(OkfError::UnknownName {
                noun: "mail job",
                ..
            })
        ));
        assert!(matches!(
            unknown_delete,
            Err(OkfError::UnknownName {
                noun: "mail job",
                ..
            })
        ));
        assert!(
            matches!(running_delete, Err(OkfError::Refused { .. })),
            "{running_delete:?}"
        );
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_keeps_a_week_of_done_and_a_month_of_failed_jobs() {
        let db = test_db().await;
        for _ in 0..4 {
            enqueue(&db.pool, "alice@example.com", &Template::Test, now())
                .await
                .unwrap();
        }
        // Age the four jobs by hand: done 8 days ago, done 6 days ago,
        // failed 31 days ago, failed 29 days ago.
        sqlx::query(
            "WITH aged AS (SELECT id, row_number() OVER (ORDER BY id) AS n FROM apalis.jobs) \
             UPDATE apalis.jobs j SET \
                 status = CASE WHEN aged.n <= 2 THEN 'Done' ELSE 'Killed' END, \
                 done_at = $1 - CASE aged.n WHEN 1 THEN interval '8 days' \
                                            WHEN 2 THEN interval '6 days' \
                                            WHEN 3 THEN interval '31 days' \
                                            ELSE interval '29 days' END \
             FROM aged WHERE j.id = aged.id",
        )
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        let pruned = prune(&db.pool, now()).await.unwrap();

        assert_eq!(pruned, 2);
        let left: Vec<String> =
            sqlx::query_scalar("SELECT status FROM apalis.jobs ORDER BY status")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(left, ["Done", "Killed"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_losing_the_database_the_worker_restarts_and_sends() {
        let db = test_db().await;
        let database: String = sqlx::query_scalar("SELECT current_database()")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // A connection to the server's `template1` database (the test's own
        // may be `postgres`): Postgres refuses to close the database a
        // connection is in, and while the test's database refuses
        // connections this is the way back in.
        // `use sqlx::Connection` brings the `connect_with` constructor into scope.
        use sqlx::Connection;
        let mut admin = sqlx::PgConnection::connect_with(
            &db.pool
                .connect_options()
                .as_ref()
                .clone()
                .database("template1"),
        )
        .await
        .unwrap();
        let mailer = Scripted::with(vec![]);
        let (shutdown, worker) = start_on(worker_pool(&db.pool), Arc::clone(&mailer), "okf-mail-r");
        for _ in 0..200 {
            let registered: i64 = sqlx::query_scalar("SELECT count(*) FROM apalis.workers")
                .fetch_one(&db.pool)
                .await
                .unwrap();
            if registered == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // The outage: no new connections, and the worker's own are cut.
        // `format!` for the name: ALTER DATABASE takes no bind parameters.
        // `AssertSqlSafe`: the name is the test database's own.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE \"{database}\" WITH ALLOW_CONNECTIONS false"
        )))
        .execute(&mut admin)
        .await
        .unwrap();
        let cut: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM (SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
             WHERE datname = $1 AND application_name = $2) AS cut",
        )
        .bind(&database)
        .bind(WORKER_APPLICATION_NAME)
        .fetch_one(&mut admin)
        .await
        .unwrap();
        // Long enough for the next poll to fail and a restart or two to fail too.
        tokio::time::sleep(Duration::from_millis(800)).await;
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "ALTER DATABASE \"{database}\" WITH ALLOW_CONNECTIONS true"
        )))
        .execute(&mut admin)
        .await
        .unwrap();
        admin.close().await.unwrap();
        enqueue(&db.pool, "alice@example.com", &Template::Test, now())
            .await
            .unwrap();
        wait_for(&db.pool, "Done", 1).await;
        let sent_by: String = sqlx::query_scalar("SELECT lock_by FROM apalis.jobs")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        stop(shutdown, worker).await;

        assert!(cut >= 1, "the worker had connections to cut");
        assert!(
            sent_by.starts_with("okf-mail-r-") && sent_by != "okf-mail-r",
            "sent by a restarted worker: {sent_by}"
        );
        assert_eq!(mailer.sent(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_worker_stops_when_told() {
        let db = test_db().await;
        let (shutdown, worker) = start(&db.pool, Scripted::with(vec![]));

        stop(shutdown, worker).await;
    }

    #[test]
    fn the_worker_name_names_this_process() {
        let name = worker_name();

        assert!(name.starts_with("okf-mail-"), "{name}");
        assert!(
            name.ends_with(&format!("-{}", std::process::id())),
            "{name}"
        );
    }

    #[test]
    fn debug_hides_the_payloads_bodies() {
        let job = SendMail {
            mail: Mail {
                to: "alice@example.com".to_string(),
                subject: "Reset your password".to_string(),
                text: "token=s3cr3t".to_string(),
                html: "token=s3cr3t".to_string(),
            },
            template: "reset".to_string(),
            queued_at: 0,
        };

        let shown = format!("{job:?}");

        assert!(
            shown.contains("reset") && !shown.contains("s3cr3t"),
            "{shown}"
        );
    }
}
