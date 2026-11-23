//! Background job worker — polls `rapina_jobs`, claims work, and dispatches
//! to registered handlers.
//!
//! # Lifecycle
//!
//! Jobs move through three states in normal execution:
//!
//! ```text
//! pending → running → completed
//!                   ↘ failed   (or back to pending if retries remain)
//! ```
//!
//! The worker atomically transitions each job from `pending` to `running`
//! using backend-specific claiming strategies:
//!
//! - **PostgreSQL:** CTE + `UPDATE … FROM` + `RETURNING` + `FOR UPDATE SKIP LOCKED`
//! - **MySQL 8.0+:** transaction with `SELECT … FOR UPDATE SKIP LOCKED`, then `UPDATE`
//! - **SQLite 3.35+:** `UPDATE … WHERE id IN (subquery) … RETURNING`
//!
//! # Crash recovery
//!
//! Jobs left `running` by a dead worker are reaped on the poll cycle after
//! their lease expires and rejoin the queue under the same retry budget as
//! failures. A panicking handler does not stop the worker: the panic is
//! caught, logged at `error` level, and routed through the same retry path.
//!
//! # Graceful shutdown
//!
//! The worker installs its own SIGINT/SIGTERM listeners (identical to
//! `server.rs`). When a signal fires the poll loop exits after the
//! **current batch** finishes — no job is abandoned mid-execution.
//!
//! # Trace propagation
//!
//! When a job row has a `trace_id`, the worker opens a tracing span that
//! includes the original value so all log lines emitted during the job share
//! the same trace identifier as the HTTP request that enqueued it.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt as _;
use sea_orm::{ConnectionTrait, DatabaseConnection};
use tracing::Instrument;

use crate::error::panic_payload_message;
use crate::jobs::backend::{Mysql, Postgres, Sqlite};
use crate::jobs::retry::{apply_failure, apply_success};
use crate::jobs::{JobDescriptor, JobRow, RetryPolicy};
use crate::state::AppState;

/// Configuration for the in-process background job worker.
///
/// All fields have sensible defaults via [`Default`] — call [`JobConfig::default()`]
/// and override only what you need:
///
/// ```rust,ignore
/// use rapina::jobs::JobConfig;
/// use std::time::Duration;
///
/// let config = JobConfig::default()
///     .queues(["default", "emails"])
///     .poll_interval(Duration::from_secs(2));
/// ```
#[derive(Debug, Clone)]
pub struct JobConfig {
    /// How often the worker wakes up to check for new jobs.
    ///
    /// Shorter intervals reduce latency at the cost of more database round-trips.
    /// Default: 5 seconds.
    pub poll_interval: Duration,
    /// Maximum number of jobs claimed in a single poll cycle.
    ///
    /// Each claimed job is executed sequentially before the next poll.
    /// Increase this to raise throughput at the cost of higher tail latency
    /// for jobs at the back of the batch.
    /// Default: 10.
    pub batch_size: i32,
    /// Queues the worker subscribes to.
    ///
    /// Only jobs whose `queue` column matches one of these names are claimed.
    /// Default: `["default"]`.
    pub queues: Vec<String>,
    /// How long a job lock is held before another worker may reclaim it.
    ///
    /// Sets `locked_until = NOW() + job_timeout` when a job is claimed.
    /// If the worker process crashes the lock expires after this duration,
    /// at which point a new worker can pick up the job.
    /// Default: 30 seconds.
    ///
    /// Reclaim happens on the poll cycle after expiry, so recovery latency is
    /// bounded by `job_timeout + poll_interval`. Set this higher than your
    /// slowest handler: a handler that outlives its lease can be started a
    /// second time while the first execution is still running, and there is
    /// no heartbeat to renew the lease mid-run.
    pub job_timeout: Duration,
    /// Per-queue overrides layered on top of the global settings above.
    ///
    /// Entries are matched to a queue by [`QueueConfig::queue_name`]; a queue
    /// with no entry here uses the global values unchanged.
    ///
    /// [`queue_configs()`](Self::queue_configs) keeps this list and `queues` in
    /// sync, subscribing any name it does not already find there. An entry can
    /// still be orphaned by a later [`queues()`](Self::queues) call, which
    /// replaces the subscription list outright — an orphaned entry is ignored.
    /// Default: empty.
    pub queue_configs: Vec<QueueConfig>,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(5),
            batch_size: 10,
            queues: vec!["default".to_string()],
            job_timeout: Duration::from_secs(30),
            queue_configs: Vec::new(),
        }
    }
}

impl JobConfig {
    /// Overrides the poll interval.
    pub fn poll_interval(mut self, interval: Duration) -> Self {
        self.poll_interval = interval;
        self
    }

    /// Overrides the per-cycle batch size.
    pub fn batch_size(mut self, size: i32) -> Self {
        self.batch_size = size;
        self
    }

    /// Overrides the list of queues to subscribe to, replacing any previous
    /// list. Duplicate names are collapsed, keeping first-seen order.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// JobConfig::default().queues(["default", "emails", "heavy"])
    /// ```
    ///
    /// # Panics
    ///
    /// Panics if the list is empty, or if any name is empty. An empty list
    /// leaves the worker subscribed to nothing, and an empty name can never
    /// match a job row because `#[job(queue = "")]` is rejected at compile
    /// time.
    pub fn queues(mut self, queues: impl IntoIterator<Item = impl Into<String>>) -> Self {
        let mut unique: Vec<String> = Vec::new();
        for q in queues {
            let q: String = q.into();
            assert!(!q.is_empty(), "queue name must not be empty");
            if !unique.contains(&q) {
                unique.push(q);
            }
        }
        assert!(!unique.is_empty(), "queues must not be empty");
        self.queues = unique;
        self
    }

    /// Registers per-queue overrides, subscribing any queue not already listed
    /// in [`queues()`](Self::queues).
    ///
    /// Accumulates across calls. A second config for the same name replaces the
    /// first outright rather than merging field by field, so a field set only on
    /// the discarded config reverts to the global value.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use rapina::jobs::{JobConfig, QueueConfig};
    /// use std::time::Duration;
    ///
    /// // `browser` claims one job at a time with a five-minute lease;
    /// // every other queue keeps the global pacing.
    /// JobConfig::default()
    ///     .batch_size(20)
    ///     .queue_configs([QueueConfig::new("browser")
    ///         .batch_size(1)
    ///         .job_timeout(Duration::from_secs(300))])
    /// ```
    pub fn queue_configs(mut self, configs: impl IntoIterator<Item = QueueConfig>) -> Self {
        for config in configs {
            if !self.queues.contains(&config.queue_name) {
                self.queues.push(config.queue_name.clone());
            }

            match self
                .queue_configs
                .iter_mut()
                .find(|c| c.queue_name == config.queue_name)
            {
                Some(existing) => *existing = config,
                None => self.queue_configs.push(config),
            }
        }
        self
    }

    /// Expands this config into one fully-merged config per subscribed queue.
    ///
    /// Every name in [`queues()`](Self::queues) contributes exactly one entry,
    /// taking each field from its [`QueueConfig`] when set and from the global
    /// value otherwise. Driving off `queues` rather than `queue_configs` is
    /// deliberate: a queue with no override still gets an entry, and an override
    /// for an unsubscribed queue is never looked up.
    pub(crate) fn resolved_queues(&self) -> Vec<ResolvedQueueConfig> {
        self.queues
            .iter()
            .map(|name| {
                let over = self.queue_configs.iter().find(|c| &c.queue_name == name);
                ResolvedQueueConfig {
                    queue: name.clone(),
                    poll_interval: over
                        .and_then(|c| c.poll_interval)
                        .unwrap_or(self.poll_interval),
                    batch_size: over.and_then(|c| c.batch_size).unwrap_or(self.batch_size),
                    job_timeout: over.and_then(|c| c.job_timeout).unwrap_or(self.job_timeout),
                }
            })
            .collect()
    }

    /// Overrides the job execution lock timeout.
    pub fn job_timeout(mut self, timeout: Duration) -> Self {
        self.job_timeout = timeout;
        self
    }
}

/// Per-queue overrides for the background job worker.
///
/// Every setting is optional: a field left unset inherits the corresponding
/// value from the global [`JobConfig`], so you declare only what differs for
/// this queue. Register it with [`JobConfig::queue_configs()`].
///
/// The motivating case is a queue whose jobs are far heavier than the rest —
/// claimed one at a time with a longer lease, while everything else keeps the
/// global pacing:
///
/// ```rust,ignore
/// use rapina::jobs::QueueConfig;
/// use std::time::Duration;
///
/// QueueConfig::new("browser")
///     .batch_size(1)
///     .job_timeout(Duration::from_secs(300))
/// ```
#[derive(Clone, Debug)]
pub struct QueueConfig {
    /// Name of the queue this config applies to, matched against the `queue`
    /// column of `rapina_jobs`.
    pub queue_name: String,
    /// Overrides the global `poll_interval` for this queue.
    pub poll_interval: Option<Duration>,
    /// Overrides the global `batch_size` for this queue.
    pub batch_size: Option<i32>,
    /// Overrides the global `job_timeout` for this queue.
    pub job_timeout: Option<Duration>,
}

impl QueueConfig {
    /// Creates an override set for `queue_name` that inherits every global
    /// setting.
    ///
    /// # Panics
    ///
    /// Panics if `queue_name` is empty.
    pub fn new(queue_name: impl Into<String>) -> Self {
        let queue_name = queue_name.into();
        assert!(!queue_name.is_empty(), "queue name must not be empty");
        Self {
            queue_name,
            poll_interval: None,
            batch_size: None,
            job_timeout: None,
        }
    }

    /// Overrides the poll interval for this queue.
    pub fn poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = Some(poll_interval);
        self
    }

    /// Overrides the per-cycle batch size for this queue.
    pub fn batch_size(mut self, batch_size: i32) -> Self {
        self.batch_size = Some(batch_size);
        self
    }

    /// Overrides the job execution lock timeout for this queue.
    pub fn job_timeout(mut self, timeout: Duration) -> Self {
        self.job_timeout = Some(timeout);
        self
    }
}

/// One queue's fully-merged settings — everything a single worker needs.
///
/// Produced by [`JobConfig::resolved_queues`]. No field is optional, so the
/// claim and reap statements never have to know that overrides exist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ResolvedQueueConfig {
    pub queue: String,
    pub poll_interval: Duration,
    pub batch_size: i32,
    pub job_timeout: Duration,
}

/// A default ResolvedQueueConfig helper for the tests
#[cfg(test)]
fn default_resolved_queue() -> ResolvedQueueConfig {
    JobConfig::default().resolved_queues().pop().unwrap()
}

/// A ResolvedQueueConfig for a named queue, for tests that exercise per-queue
/// settings. Built through the real merge so it cannot drift from it.
// Only the database-backed tests need a non-default queue.
#[cfg(all(test, feature = "sqlite"))]
fn resolved_queue(queue: &str, batch_size: i32) -> ResolvedQueueConfig {
    JobConfig::default()
        .queue_configs([QueueConfig::new(queue).batch_size(batch_size)])
        .resolved_queues()
        .into_iter()
        .find(|r| r.queue == queue)
        .expect("the queue registered above always resolves")
}

/// Runs background job workers until SIGINT or SIGTERM, with no HTTP server.
///
/// This is the entry point for running workers as their own process, separate
/// from the HTTP app and sharing only the database. One worker is spawned per
/// queue in `config` — see [`JobConfig::queues`] and
/// [`JobConfig::queue_configs`] — and each polls, claims and reaps only its own
/// queue, so workers in different processes never contend over the same leases.
///
/// Returns once every worker has drained: on a shutdown signal each finishes the
/// batch it is executing and then stops, so no job is abandoned in `running`.
///
/// # Handlers must be linked into the calling binary
///
/// `#[job]` registers handlers with [`inventory`](crate::inventory) at link
/// time, so the dispatch table is whatever the *final executable* contains. Call
/// this from a binary in the crate that defines your jobs — typically
/// `src/bin/worker.rs`. A binary that does not link those definitions finds no
/// handler for any `job_type` and permanently fails every job it claims.
///
/// # Dependency injection
///
/// Job handlers taking [`Db`](crate::database::Db) work here: the connection you
/// pass in is registered in the worker's state. Handlers taking
/// [`State<T>`](crate::extract::State) do **not** — this entry point has no way
/// to receive those values, and such a handler fails every attempt until its
/// retries are exhausted. Keep those jobs in the in-process worker started by
/// [`Rapina::jobs`](crate::app::Rapina::jobs).
///
/// # Example
///
/// ```rust,ignore
/// // src/bin/worker.rs
/// use rapina::database::DatabaseConfig;
/// use rapina::jobs::{run_worker, JobConfig, QueueConfig};
/// use rapina::prelude::*;
/// use std::time::Duration;
///
/// // Pull in the crate that defines the `#[job]` handlers so they are linked.
/// use my_app::jobs::*;
///
/// #[tokio::main]
/// async fn main() -> Result<(), Box<dyn std::error::Error>> {
///     // Nothing installs a subscriber for you here, unlike `listen()`.
///     TracingConfig::new().init();
///
///     let db = DatabaseConfig::from_env()?.connect().await?;
///
///     run_worker(
///         db,
///         JobConfig::default().queues(["browser"]).queue_configs([
///             QueueConfig::new("browser")
///                 .batch_size(1)
///                 .job_timeout(Duration::from_secs(300)),
///         ]),
///     )
///     .await;
///
///     Ok(())
/// }
/// ```
pub async fn run_worker(db: DatabaseConnection, config: JobConfig) {
    let state = Arc::new(AppState::new().with(db));

    let handles: Vec<_> = config
        .resolved_queues()
        .into_iter()
        .map(|queue| tokio::spawn(Worker::new(state.clone(), queue).run()))
        .collect();

    tracing::info!(workers = handles.len(), "Standalone job workers started");

    // Each worker watches for SIGINT/SIGTERM itself and returns after its
    // current batch, so joining here is what keeps the process alive and what
    // makes the drain observable to the caller.
    for result in futures_util::future::join_all(handles).await {
        if let Err(e) = result {
            tracing::error!(error = %e, "Job worker task did not shut down cleanly");
        }
    }
}

/// The background worker that drives the job queue.
///
/// Constructed internally by [`Rapina::jobs`](crate::app::Rapina::jobs) and
/// [`run_worker`], and spawned via `tokio::spawn`. Not intended for direct
/// construction outside the framework.
pub(crate) struct Worker {
    /// Shared application state passed to every job handler for DI.
    state: Arc<AppState>,
    /// Worker configuration (queues, intervals, timeouts).
    config: ResolvedQueueConfig,
}

impl Worker {
    /// Creates a new worker with the given state and configuration.
    pub(crate) fn new(state: Arc<AppState>, config: ResolvedQueueConfig) -> Self {
        Self { state, config }
    }

    /// Runs the poll loop until a shutdown signal is received.
    ///
    /// The loop polls immediately on startup, executes the claimed batch, then
    /// sleeps for `poll_interval`. SIGINT and SIGTERM both break the loop after
    /// the current batch completes — no job is left in `running` state on a
    /// clean shutdown.
    pub(crate) async fn run(self) {
        let mut ctrl_c = pin!(tokio::signal::ctrl_c());

        // Platform-specific SIGTERM future.  On non-Unix targets (Windows) this
        // future never resolves so only ctrl-c triggers shutdown.
        let mut sigterm: std::pin::Pin<Box<dyn Future<Output = ()> + Send>> = Box::pin(async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::SignalKind;
                tokio::signal::unix::signal(SignalKind::terminate())
                    .expect("failed to install SIGTERM handler")
                    .recv()
                    .await;
            }
            #[cfg(not(unix))]
            {
                std::future::pending::<()>().await;
            }
        });

        tracing::info!(
            queue = ?self.config.queue,
            poll_interval_secs = self.config.poll_interval.as_secs(),
            "Job worker started"
        );

        let Some(db) = self.state.get::<DatabaseConnection>() else {
            tracing::error!(
                "Job worker: no DatabaseConnection in AppState — worker will not start. \
                             Call .with_database() before .jobs()."
            );
            return;
        };

        loop {
            // Recover jobs stranded in `running` by a dead worker before
            // claiming, so a reaped row is picked up in the same cycle.
            match reap_expired(db, &self.config.queue).await {
                Ok((failed, reclaimed)) if failed + reclaimed > 0 => {
                    tracing::warn!(failed, reclaimed, "Reaped expired job leases");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::error!(error = %e, "Failed to reap expired job leases");
                }
            }

            // Claim and execute a batch before sleeping so jobs enqueued just
            // before startup are processed without an initial delay.
            match claim_batch(db, &self.config).await {
                Ok(jobs) => {
                    let n = jobs.len();
                    if n > 0 {
                        tracing::debug!(claimed = n, "Claimed job batch");
                    }
                    for job in jobs {
                        self.dispatch(db, job).await;
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "Failed to claim jobs from database");
                }
            }

            // Wait for the next poll tick or a shutdown signal.
            tokio::select! {
                _ = tokio::time::sleep(self.config.poll_interval) => {}
                _ = ctrl_c.as_mut() => {
                    tracing::info!("Job worker received shutdown signal, stopping.");
                    break;
                }
                _ = sigterm.as_mut() => {
                    tracing::info!("Job worker received shutdown signal, stopping.");
                    break;
                }
            }
        }
    }

    /// Dispatches a single claimed job to its registered handler.
    ///
    /// Looks up the handler by `job_type` in the `inventory` registry.  If no
    /// handler is found the job is permanently failed immediately (no retry).
    /// Otherwise the handler is called and the result is forwarded to
    /// [`apply_success`] or [`apply_failure`].
    async fn dispatch(&self, db: &DatabaseConnection, job: JobRow) {
        let handler = inventory::iter::<JobDescriptor>
            .into_iter()
            .find(|d| d.job_type == job.job_type);

        let Some(descriptor) = handler else {
            tracing::warn!(
                job_id = %job.id,
                job_type = %job.job_type,
                "No handler registered for job type — permanently failing job"
            );
            // max_retries = 0 forces apply_failure to mark the job as failed
            // immediately without scheduling a retry.
            let _ = apply_failure(
                db,
                job.id,
                &format!("no handler registered for job type: {}", job.job_type),
                job.attempts,
                0,
                &RetryPolicy::None,
            )
            .await;
            return;
        };

        // Restore the original trace context so log lines from the handler are
        // correlated with the HTTP request that enqueued the job.
        let span = tracing::info_span!(
            "job",
            job_type = %job.job_type,
            job_id   = %job.id,
            trace_id = job.trace_id.as_deref().unwrap_or(""),
        );

        // catch_unwind keeps a panicking handler from unwinding through the
        // poll loop and killing the worker. No-op under panic=abort: the
        // process still dies.
        let outcome = std::panic::AssertUnwindSafe((descriptor.handle)(
            job.payload.clone(),
            self.state.clone(),
        ))
        .catch_unwind()
        .instrument(span)
        .await;
        let (result, panicked) = match outcome {
            Ok(r) => (r, false),
            Err(p) => (
                Err(crate::error::Error::internal(format!(
                    "job handler panicked: {}",
                    panic_payload_message(&*p)
                ))),
                true,
            ),
        };
        // Policy type and base delay come from the descriptor (set by `#[job]`
        // attributes at compile time); max_retries comes from the job row.
        let policy = build_policy(
            descriptor.retry_policy,
            job.max_retries,
            descriptor.retry_delay_secs,
        );

        match result {
            Ok(()) => {
                tracing::debug!(job_id = %job.id, job_type = %job.job_type, "Job completed");
                if let Err(e) = apply_success(db, job.id).await {
                    tracing::error!(job_id = %job.id, error = %e, "Failed to mark job as completed");
                }
            }
            Err(e) => {
                if panicked {
                    tracing::error!(job_id = %job.id, job_type = %job.job_type, error = %e, "Job handler panicked");
                } else {
                    tracing::warn!(job_id = %job.id, job_type = %job.job_type, error = %e, "Job failed");
                }
                if let Err(db_err) = apply_failure(
                    db,
                    job.id,
                    &e.to_string(),
                    job.attempts,
                    job.max_retries,
                    &policy,
                )
                .await
                {
                    tracing::error!(job_id = %job.id, error = %db_err, "Failed to record job failure");
                }
            }
        }
    }
}

/// Constructs a [`RetryPolicy`] from the descriptor's compile-time attributes
/// and the job row's `max_retries`.
fn build_policy(retry_policy: &str, max_retries: i32, delay_secs: f64) -> RetryPolicy {
    let delay = Duration::from_secs_f64(delay_secs);
    match retry_policy {
        "fixed" => RetryPolicy::fixed(max_retries, delay),
        "none" => RetryPolicy::none(),
        _ => RetryPolicy::exponential(max_retries, delay),
    }
}

/// Claims up to `config.batch_size` jobs from the subscribed queues in a
/// single atomic statement and returns their rows.
///
/// Uses backend-specific claiming strategies:
/// - **PostgreSQL:** CTE + `UPDATE … FROM` + `RETURNING` + `FOR UPDATE SKIP LOCKED`
/// - **MySQL 8.0+:** transaction with `SELECT … FOR UPDATE SKIP LOCKED`, then `UPDATE`
/// - **SQLite 3.35+:** `UPDATE … WHERE id IN (subquery) … RETURNING` (single-writer, no SKIP LOCKED needed)
async fn claim_batch(
    db: &DatabaseConnection,
    config: &ResolvedQueueConfig,
) -> Result<Vec<JobRow>, sea_orm::DbErr> {
    match db.get_database_backend() {
        sea_orm::DbBackend::Postgres => Postgres::claim_batch(db, config).await,
        sea_orm::DbBackend::MySql => Mysql::claim_batch(db, config).await,
        sea_orm::DbBackend::Sqlite => Sqlite::claim_batch(db, config).await,
    }
}

/// Recovers jobs stranded in `running` by a dead worker.
///
/// Returns `(failed, reclaimed)`. A worker that dies mid-execution (crash,
/// SIGKILL, power loss) leaves its rows in `running` with a stale
/// `locked_until`; the claim only reads `pending`, so without this pass those
/// rows are stranded forever. Reaped executions count toward `attempts`
/// exactly like errored runs in [`apply_failure`], so crash-loops are bounded
/// by the same `max_retries`.
async fn reap_expired(db: &DatabaseConnection, queue: &str) -> Result<(u64, u64), sea_orm::DbErr> {
    match db.get_database_backend() {
        sea_orm::DbBackend::Postgres => Postgres::reap_expired(db, queue).await,
        sea_orm::DbBackend::MySql => Mysql::reap_expired(db, queue).await,
        sea_orm::DbBackend::Sqlite => Sqlite::reap_expired(db, queue).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::state::AppState;

    // ── retry policy resolution ───────────────────────────────────────────────

    #[test]
    fn descriptor_exponential_produces_exponential_policy() {
        let policy = build_policy("exponential", 5, 2.0);
        assert!(matches!(
            policy,
            RetryPolicy::Exponential { max_retries: 5, .. }
        ));
    }

    #[test]
    fn descriptor_fixed_produces_fixed_policy() {
        let policy = build_policy("fixed", 3, 30.0);
        assert!(matches!(policy, RetryPolicy::Fixed { max_retries: 3, .. }));
    }

    #[test]
    fn descriptor_none_produces_none_policy() {
        let policy = build_policy("none", 0, 0.0);
        assert!(matches!(policy, RetryPolicy::None));
    }

    #[test]
    fn descriptor_unknown_policy_falls_back_to_exponential() {
        let policy = build_policy("bogus", 3, 1.0);
        assert!(matches!(policy, RetryPolicy::Exponential { .. }));
    }

    #[test]
    fn descriptor_base_delay_is_forwarded() {
        let policy = build_policy("exponential", 3, 5.0);
        match policy {
            RetryPolicy::Exponential { base_delay, .. } => {
                assert_eq!(base_delay, Duration::from_secs(5));
            }
            _ => panic!("expected Exponential"),
        }
    }

    #[test]
    fn descriptor_fixed_delay_is_forwarded() {
        let policy = build_policy("fixed", 3, 20.0);
        match policy {
            RetryPolicy::Fixed { delay, .. } => {
                assert_eq!(delay, Duration::from_secs(20));
            }
            _ => panic!("expected Fixed"),
        }
    }

    // ── no-database exit ─────────────────────────────────────────────────────

    /// Worker must not block forever when no `DatabaseConnection` is registered
    /// in `AppState`.  It should log an error and return immediately so the
    /// spawned task doesn't leak.
    #[tokio::test]
    async fn worker_exits_immediately_without_database() {
        let state = Arc::new(AppState::new()); // no DB registered
        let worker = Worker::new(state, default_resolved_queue());
        let handle = tokio::spawn(worker.run());

        let result = tokio::time::timeout(Duration::from_millis(500), handle).await;
        assert!(
            result.is_ok(),
            "worker should return quickly when no DB is in AppState"
        );
        assert!(result.unwrap().is_ok(), "worker task should not panic");
    }

    #[test]
    fn job_config_defaults() {
        let config = JobConfig::default();
        assert_eq!(config.poll_interval, Duration::from_secs(5));
        assert_eq!(config.batch_size, 10);
        assert_eq!(config.queues, vec!["default"]);
        assert_eq!(config.job_timeout, Duration::from_secs(30));
    }

    #[test]
    fn job_config_builder_methods() {
        let config = JobConfig::default()
            .poll_interval(Duration::from_secs(2))
            .batch_size(5)
            .queues(["emails", "default"])
            .job_timeout(Duration::from_secs(60));

        assert_eq!(config.poll_interval, Duration::from_secs(2));
        assert_eq!(config.batch_size, 5);
        assert_eq!(config.queues, vec!["emails", "default"]);
        assert_eq!(config.job_timeout, Duration::from_secs(60));
    }

    #[test]
    #[should_panic(expected = "queue name must not be empty")]
    fn job_config_queue_without_name_panics() {
        JobConfig::default().queues(["default", ""]);
    }
    #[test]
    #[should_panic(expected = "queues must not be empty")]
    fn job_config_with_empty_queues_panics() {
        JobConfig::default().queues(Vec::<String>::new());
    }

    #[test]
    fn job_config_queue_configs_methods_adds_queue_if_not_already_registered() {
        let config = JobConfig::default()
            .queues(["default"])
            .queue_configs([QueueConfig::new("test")]);

        assert_eq!(config.queues.len(), 2);
        assert!(config.queues.contains(&String::from("test")));
    }

    #[test]
    fn job_config_queue_configs_method_pushes_config_to_queue_configs() {
        let config = JobConfig::default().queue_configs([QueueConfig::new("default")]);

        assert_eq!(config.queue_configs.len(), 1);
        assert!(
            config
                .queue_configs
                .iter()
                .any(|c| c.queue_name == "default")
        );
    }

    #[test]
    fn job_config_queue_config_method_overwrites_queue_config_if_already_registered() {
        let config = JobConfig::default().queue_configs([
            QueueConfig::new("default"),
            QueueConfig::new("default").batch_size(1),
        ]);

        assert_eq!(config.queue_configs.len(), 1);
        assert!(config.queue_configs.iter().any(|c| c.batch_size == Some(1)));
    }

    #[test]
    fn job_config_queue_name_dedupe() {
        let config = JobConfig::default().queues(["default", "default"]);

        assert_eq!(config.queues.len(), 1);
    }

    #[test]
    fn queue_config_builder_properties_are_all_none_except_name() {
        let config = QueueConfig::new("test");

        assert_eq!(config.queue_name, String::from("test"));
        assert_eq!(config.poll_interval, None);
        assert_eq!(config.batch_size, None);
        assert_eq!(config.job_timeout, None);
    }

    #[test]
    fn queue_config_builder_methods() {
        let config = QueueConfig::new("test")
            .poll_interval(Duration::from_secs(4))
            .batch_size(3)
            .job_timeout(Duration::from_secs(60));

        assert_eq!(config.poll_interval, Some(Duration::from_secs(4)));
        assert_eq!(config.batch_size, Some(3));
        assert_eq!(config.job_timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn job_config_resolved_queues_method_covers_every_subscribed_queue() {
        let config = JobConfig::default()
            .batch_size(20)
            .poll_interval(Duration::from_secs(2))
            .queues(["default"])
            .queue_configs([QueueConfig::new("test")
                .batch_size(1)
                .poll_interval(Duration::from_secs(7))]);

        let resolved = config.resolved_queues();

        assert_eq!(resolved.len(), 2);

        let default = resolved.iter().find(|q| q.queue == "default").unwrap();

        assert_eq!(default.poll_interval, config.poll_interval);
        assert_eq!(default.batch_size, config.batch_size);
        assert_eq!(default.job_timeout, config.job_timeout);

        let test = resolved.iter().find(|x| x.queue == "test").unwrap();

        assert_eq!(test.poll_interval, Duration::from_secs(7));
        assert_eq!(test.batch_size, 1);
        assert_eq!(test.job_timeout, config.job_timeout);
    }

    #[test]
    fn queue_config_replacement_for_same_named_config() {
        let config = JobConfig::default().queue_configs([
            QueueConfig::new("default").poll_interval(Duration::from_secs(9)),
            QueueConfig::new("default").batch_size(1),
        ]);

        let resolve = config.resolved_queues();

        assert_eq!(resolve.len(), 1);

        assert_eq!(resolve[0].poll_interval, config.poll_interval);
        assert_eq!(resolve[0].batch_size, 1);
    }

    #[test]
    #[should_panic(expected = "queue name must not be empty")]
    fn queue_config_builder_without_name_panics() {
        QueueConfig::new("");
    }
}

#[cfg(all(test, feature = "sqlite"))]
mod sqlite_tests {
    use std::future::Future;
    use std::pin::Pin;

    use sea_orm::{ConnectionTrait, Database, FromQueryResult, Statement};
    use uuid::Uuid;

    use crate::jobs::backend::Sqlite;
    use crate::jobs::retry::{apply_failure, apply_success};
    use crate::jobs::{JobDescriptor, JobRequest, JobResult, JobRow, RapinaJobs};
    use crate::jobs::{create_rapina_jobs, reap_indexes};
    use crate::state::AppState;
    use serial_test::serial;

    use super::*;

    crate::migrations! {
        create_rapina_jobs,
        reap_indexes,
    }

    async fn jobs_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        crate::migration::run_pending::<Migrator>(&db)
            .await
            .unwrap();
        db
    }

    async fn insert_job(
        db: &DatabaseConnection,
        job_type: &'static str,
        queue: &'static str,
        max_retries: i32,
    ) -> Uuid {
        let id = Uuid::new_v4();
        let stmt = Sqlite::build_insert_stmt(
            JobRequest {
                job_type,
                payload: serde_json::json!({}),
                queue,
                max_retries,
            },
            None,
            id,
        );
        db.execute(stmt).await.unwrap();
        id
    }

    async fn fetch_row(db: &DatabaseConnection, id: Uuid) -> JobRow {
        let stmt = Statement::from_sql_and_values(
            sea_orm::DbBackend::Sqlite,
            format!(
                "SELECT * FROM {} WHERE {} = ?",
                RapinaJobs::table_name(),
                RapinaJobs::id()
            ),
            [sea_orm::Value::String(Some(Box::new(id.to_string())))],
        );
        let row = db.query_one(stmt).await.unwrap().expect("job row missing");
        JobRow::from_query_result(&row, "").unwrap()
    }

    /// Puts a row in the exact state a worker killed with SIGKILL leaves
    /// behind: `running` with a `locked_until` ten seconds in the past.
    /// Same `datetime('now')` format the claim statements write.
    async fn simulate_crash(db: &DatabaseConnection, id: Uuid, attempts: i32) {
        db.execute_unprepared(&format!(
            "UPDATE {} SET status='running', attempts={}, \
             started_at=datetime('now'), locked_until=datetime('now','-10 seconds') \
             WHERE id='{}'",
            RapinaJobs::table_name(),
            attempts,
            id
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn reaper_reclaims_expired_lease_and_claim_picks_it_up() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_job", "default", 3).await;
        simulate_crash(&db, id, 0).await;

        let (failed, reclaimed) = reap_expired(&db, "default").await.unwrap();
        assert_eq!((failed, reclaimed), (0, 1));

        let claimed = claim_batch(&db, &default_resolved_queue()).await.unwrap();
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, id);
        assert_eq!(claimed[0].status, "running");
        assert_eq!(claimed[0].attempts, 1);
    }

    #[tokio::test]
    async fn reaper_fails_expired_job_at_retry_boundary() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_job", "default", 3).await;
        simulate_crash(&db, id, 2).await;

        let (failed, reclaimed) = reap_expired(&db, "default").await.unwrap();
        assert_eq!((failed, reclaimed), (1, 0));

        let row = fetch_row(&db, id).await;
        assert_eq!(row.status, "failed");
        assert_eq!(row.attempts, 3);
        assert!(row.finished_at.is_some());
        assert!(row.last_error.as_deref().unwrap_or("").contains("lease"));

        assert!(
            claim_batch(&db, &default_resolved_queue())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn reaper_ignores_live_lease() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_job", "default", 3).await;
        db.execute_unprepared(&format!(
            "UPDATE {} SET status='running', started_at=datetime('now'), \
             locked_until=datetime('now','+60 seconds') WHERE id='{}'",
            RapinaJobs::table_name(),
            id
        ))
        .await
        .unwrap();

        let (failed, reclaimed) = reap_expired(&db, "default").await.unwrap();
        assert_eq!((failed, reclaimed), (0, 0));

        let row = fetch_row(&db, id).await;
        assert_eq!(row.status, "running");
        assert!(row.locked_until.is_some());

        assert!(
            claim_batch(&db, &default_resolved_queue())
                .await
                .unwrap()
                .is_empty()
        );
    }

    fn panicking_handler(
        _payload: serde_json::Value,
        _state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = JobResult> + Send>> {
        Box::pin(async { panic!("boom") })
    }

    fn healthy_handler(
        _payload: serde_json::Value,
        _state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = JobResult> + Send>> {
        Box::pin(async { Ok(()) })
    }

    inventory::submit! {
        JobDescriptor {
            job_type: "reap_test_panicking_job",
            handle: panicking_handler,
            retry_policy: "exponential",
            retry_delay_secs: 0.0,
        }
    }

    inventory::submit! {
        JobDescriptor {
            job_type: "reap_test_healthy_job",
            handle: healthy_handler,
            retry_policy: "exponential",
            retry_delay_secs: 0.0,
        }
    }

    /// The panic tests swap the process-global hook, so they must not run
    /// concurrently or the no-op hook can leak into other tests. Async mutex
    /// because the guard has to live across the test's awaits.
    static PANIC_HOOK_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[tokio::test]
    async fn panicking_job_is_retried_and_healthy_job_still_dispatches() {
        let _hook_guard = PANIC_HOOK_LOCK.lock().await;
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_panicking_job", "default", 3).await;

        let worker = Worker::new(Arc::new(AppState::new()), default_resolved_queue());
        let job = claim_batch(&db, &default_resolved_queue())
            .await
            .unwrap()
            .remove(0);
        worker.dispatch(&db, job).await;

        let row = fetch_row(&db, id).await;
        assert_eq!(row.status, "pending");
        assert_eq!(row.attempts, 1);
        assert!(row.last_error.as_deref().unwrap_or("").contains("panicked"));

        // The loop survived: a healthy job still dispatches to completion.
        // The panicking row is claimable again too, so dispatch everything
        // the batch returns and assert on the healthy row only.
        let healthy_id = insert_job(&db, "reap_test_healthy_job", "default", 3).await;
        for job in claim_batch(&db, &default_resolved_queue()).await.unwrap() {
            worker.dispatch(&db, job).await;
        }
        let row = fetch_row(&db, healthy_id).await;
        assert_eq!(row.status, "completed");

        std::panic::set_hook(default_hook);
    }

    #[tokio::test]
    async fn terminal_writes_are_fenced_to_running_rows() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_job", "default", 3).await;

        // Never claimed, so the row is still `pending`: a stale worker's
        // failure or completion write must be a no-op instead of stomping it.
        apply_failure(
            &db,
            id,
            "stale write",
            0,
            3,
            &RetryPolicy::exponential(3, Duration::ZERO),
        )
        .await
        .unwrap();
        apply_success(&db, id).await.unwrap();

        let row = fetch_row(&db, id).await;
        assert_eq!(row.status, "pending");
        assert_eq!(row.attempts, 0);
        assert!(row.finished_at.is_none());
        assert!(row.last_error.is_none());
    }

    #[tokio::test]
    async fn panicking_job_at_retry_boundary_is_failed() {
        let _hook_guard = PANIC_HOOK_LOCK.lock().await;
        let default_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_panicking_job", "default", 3).await;
        db.execute_unprepared(&format!(
            "UPDATE {} SET attempts=2 WHERE id='{}'",
            RapinaJobs::table_name(),
            id
        ))
        .await
        .unwrap();

        let worker = Worker::new(Arc::new(AppState::new()), default_resolved_queue());
        let job = claim_batch(&db, &default_resolved_queue())
            .await
            .unwrap()
            .remove(0);
        worker.dispatch(&db, job).await;

        let row = fetch_row(&db, id).await;
        assert_eq!(row.status, "failed");
        assert_eq!(row.attempts, 3);
        assert!(row.finished_at.is_some());

        std::panic::set_hook(default_hook);
    }

    // ── per-queue settings ───────────────────────────────────────────────────

    /// A queue whose resolved `batch_size` is 1 must claim one row per cycle,
    /// even with a backlog, while the global default would have claimed 10.
    #[tokio::test]
    async fn claim_respects_per_queue_batch_size() {
        let db = jobs_db().await;
        for _ in 0..3 {
            insert_job(&db, "reap_test_job", "browser", 3).await;
        }

        let claimed = claim_batch(&db, &resolved_queue("browser", 1))
            .await
            .unwrap();

        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].queue, "browser");
    }

    // ── standalone entry point ───────────────────────────────────────────────

    /// Mirrors what `#[job]` generates for a handler taking `Db`: build synthetic
    /// parts, run the real extractor, propagate its error.
    fn db_requiring_handler(
        _payload: serde_json::Value,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = JobResult> + Send>> {
        Box::pin(async move {
            let (parts, _) = crate::http::Request::new(()).into_parts();
            let params = crate::extract::PathParams::new();
            let _db =
                <crate::database::Db as crate::extract::FromRequestParts>::from_request_parts(
                    &parts, &params, &state,
                )
                .await?;
            Ok(())
        })
    }

    /// Same, for a handler taking `State<String>` — a value a standalone worker
    /// has no way to receive.
    fn state_requiring_handler(
        _payload: serde_json::Value,
        state: Arc<AppState>,
    ) -> Pin<Box<dyn Future<Output = JobResult> + Send>> {
        Box::pin(async move {
            let (parts, _) = crate::http::Request::new(()).into_parts();
            let params = crate::extract::PathParams::new();
            let _unavailable = <crate::extract::State<String> as crate::extract::FromRequestParts>::from_request_parts(
                &parts, &params, &state,
            )
            .await?;
            Ok(())
        })
    }

    inventory::submit! {
        JobDescriptor {
            job_type: "run_worker_test_db_job",
            handle: db_requiring_handler,
            retry_policy: "exponential",
            retry_delay_secs: 0.0,
        }
    }

    inventory::submit! {
        JobDescriptor {
            job_type: "run_worker_test_state_job",
            handle: state_requiring_handler,
            retry_policy: "exponential",
            retry_delay_secs: 0.0,
        }
    }

    /// Polls `id` until it reaches `want`, then returns the last status seen so a
    /// failing assertion shows what it actually settled on.
    async fn await_status(db: &DatabaseConnection, id: Uuid, want: &str) -> String {
        let mut status = String::new();
        for _ in 0..200 {
            status = fetch_row(db, id).await.status;
            if status == want {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        status
    }

    fn fast_config(queues: &[&str]) -> JobConfig {
        JobConfig::default()
            .queues(queues.iter().copied())
            .poll_interval(Duration::from_millis(5))
    }

    /// `run_worker` must claim and dispatch with no HTTP server and no AppState
    /// supplied by the caller: it builds the state from the connection it is
    /// given, and handlers come from link-time registration.
    #[tokio::test]
    #[serial]
    async fn run_worker_dispatches_without_an_http_server() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_healthy_job", "default", 3).await;

        let worker = tokio::spawn(run_worker(db.clone(), fast_config(&["default"])));
        let status = await_status(&db, id, "completed").await;
        worker.abort();

        assert_eq!(
            status, "completed",
            "run_worker should have dispatched the job"
        );
    }

    /// One worker per subscribed queue, not just the first — and the call must
    /// stay alive while they run rather than spawning and returning.
    #[tokio::test]
    #[serial]
    async fn run_worker_spawns_one_worker_per_queue_and_keeps_running() {
        let db = jobs_db().await;
        let on_default = insert_job(&db, "reap_test_healthy_job", "default", 3).await;
        let on_emails = insert_job(&db, "reap_test_healthy_job", "emails", 3).await;

        let worker = tokio::spawn(run_worker(db.clone(), fast_config(&["default", "emails"])));

        let default_status = await_status(&db, on_default, "completed").await;
        let emails_status = await_status(&db, on_emails, "completed").await;

        assert_eq!(default_status, "completed");
        assert_eq!(
            emails_status, "completed",
            "the second queue needs its own worker"
        );
        assert!(
            !worker.is_finished(),
            "run_worker must join its workers, not spawn them and return"
        );

        worker.abort();
    }

    // ── unhappy paths ────────────────────────────────────────────────────────

    /// A job whose `job_type` has no registered handler is failed permanently on
    /// the first attempt. This is what a worker binary that does not link the
    /// crate defining the handlers would do to every job it claims.
    #[tokio::test]
    #[serial]
    async fn unknown_job_type_is_failed_without_retry() {
        let db = jobs_db().await;
        let id = insert_job(&db, "run_worker_test_unregistered_job", "default", 3).await;

        let worker = tokio::spawn(run_worker(db.clone(), fast_config(&["default"])));
        let status = await_status(&db, id, "failed").await;
        worker.abort();

        assert_eq!(status, "failed", "an unknown job type must not be retried");

        let row = fetch_row(&db, id).await;
        assert_eq!(row.attempts, 1);
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("run_worker_test_unregistered_job"),
            "the error should name the job type, got {:?}",
            row.last_error
        );
        assert!(row.finished_at.is_some());
    }

    /// `Db` resolves in a standalone worker, because `run_worker` registers the
    /// connection it was handed.
    #[tokio::test]
    #[serial]
    async fn db_extractor_resolves_in_a_standalone_worker() {
        let db = jobs_db().await;
        let id = insert_job(&db, "run_worker_test_db_job", "default", 3).await;

        let worker = tokio::spawn(run_worker(db.clone(), fast_config(&["default"])));
        let status = await_status(&db, id, "completed").await;
        worker.abort();

        assert_eq!(status, "completed");
    }

    /// `State<T>` does not, because the entry point has no way to receive it.
    /// The job must fail through the normal retry path rather than taking the
    /// worker down — which is why the docs say to keep such jobs in-process.
    #[tokio::test]
    #[serial]
    async fn state_extractor_fails_in_a_standalone_worker() {
        let db = jobs_db().await;
        let id = insert_job(&db, "run_worker_test_state_job", "default", 1).await;

        let worker = tokio::spawn(run_worker(db.clone(), fast_config(&["default"])));
        let status = await_status(&db, id, "failed").await;

        assert_eq!(status, "failed", "a State<T> handler cannot resolve here");
        assert!(
            !worker.is_finished(),
            "a failing handler must not take the worker down"
        );
        worker.abort();

        let row = fetch_row(&db, id).await;
        assert!(
            row.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("State not registered"),
            "expected the extractor error, got {:?}",
            row.last_error
        );
    }

    /// A worker must not reclaim leases belonging to queues it does not poll —
    /// the property that makes running workers as separate processes safe.
    #[tokio::test]
    async fn reaper_ignores_other_queues() {
        let db = jobs_db().await;
        let emails = insert_job(&db, "reap_test_job", "emails", 3).await;
        simulate_crash(&db, emails, 0).await;

        let (failed, reclaimed) = reap_expired(&db, "browser").await.unwrap();

        assert_eq!((failed, reclaimed), (0, 0), "nothing on `browser` to reap");

        let row = fetch_row(&db, emails).await;
        assert_eq!(row.status, "running", "the `emails` lease is left alone");
        assert_eq!(row.attempts, 0, "and its attempt count is untouched");
    }

    /// Raising a signal hits the whole test process, which is why every test in
    /// this module that spawns a worker is `#[serial]`. Mirrors `server.rs`.
    #[cfg(unix)]
    mod unix_tests {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::getpid;

        pub(super) fn send_sigterm() {
            kill(getpid(), Signal::SIGTERM).unwrap();
        }
    }

    /// The drain contract: on SIGTERM each worker stops after its current batch
    /// and `run_worker` returns, so the process can exit. `handle.abort()` in the
    /// other tests proves nothing about this — only a real signal does.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn run_worker_drains_and_returns_on_sigterm() {
        let db = jobs_db().await;
        let id = insert_job(&db, "reap_test_healthy_job", "default", 3).await;

        // A 60s poll interval parks the worker in its `select!` after the first
        // batch, so a prompt return can only have come from the signal arm — not
        // from a poll tick that happened to fire.
        let worker = tokio::spawn(run_worker(
            db.clone(),
            JobConfig::default()
                .queues(["default"])
                .poll_interval(Duration::from_secs(60)),
        ));

        // Wait for real work before signalling: the SIGTERM listener is only
        // registered when the poll loop first reaches its `select!`, which is
        // after the first batch has been dispatched.
        assert_eq!(await_status(&db, id, "completed").await, "completed");
        tokio::time::sleep(Duration::from_millis(50)).await;

        unix_tests::send_sigterm();

        let result = tokio::time::timeout(Duration::from_secs(5), worker).await;
        assert!(
            result.is_ok(),
            "run_worker should return once its workers drain, not hang"
        );
        assert!(
            result.unwrap().is_ok(),
            "the worker task should finish cleanly, not panic"
        );
    }

    /// Counts how many of `ids` currently sit in `status`.
    async fn count_with_status(db: &DatabaseConnection, ids: &[Uuid], status: &str) -> usize {
        let mut n = 0;
        for id in ids {
            if fetch_row(db, *id).await.status == status {
                n += 1;
            }
        }
        n
    }

    /// The feature's whole chain in one test: a `QueueConfig` on the builder is
    /// resolved, handed to that queue's own worker, and ends up in that worker's
    /// claim statement — while the other queue keeps the global batch size.
    ///
    /// The 60s poll interval is what makes this deterministic: each worker gets
    /// exactly one claim cycle, so the row counts afterwards are the batch sizes.
    #[cfg(unix)]
    #[tokio::test]
    #[serial]
    async fn run_worker_honours_per_queue_batch_size_end_to_end() {
        let db = jobs_db().await;

        let mut bulk = Vec::new();
        let mut browser = Vec::new();
        for _ in 0..3 {
            bulk.push(insert_job(&db, "reap_test_healthy_job", "bulk", 3).await);
            browser.push(insert_job(&db, "reap_test_healthy_job", "browser", 3).await);
        }

        let config = JobConfig::default()
            .queues(["bulk"])
            .batch_size(10)
            .poll_interval(Duration::from_secs(60))
            .queue_configs([QueueConfig::new("browser").batch_size(1)]);

        let worker = tokio::spawn(run_worker(db.clone(), config));

        // `bulk` inherits the global batch of 10, so one cycle takes all three.
        for id in &bulk {
            assert_eq!(await_status(&db, *id, "completed").await, "completed");
        }

        // `browser` overrides it to 1, so one cycle takes exactly one.
        for _ in 0..200 {
            if count_with_status(&db, &browser, "completed").await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        worker.abort();

        assert_eq!(
            count_with_status(&db, &browser, "completed").await,
            1,
            "the per-queue batch_size of 1 should have limited the claim to one job"
        );
        assert_eq!(
            count_with_status(&db, &browser, "pending").await,
            2,
            "the rest of the browser backlog should be untouched"
        );
    }
}
