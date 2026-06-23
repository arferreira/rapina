use std::time::Duration;

use sea_orm::{ConnectionTrait, DbBackend};
use uuid::Uuid;

use crate::jobs::backend::{Mysql, Postgres, Sqlite};

/// Controls how a failed job is retried.
///
/// The policy determines the delay between attempts and the maximum number
/// of times a job will be executed before being permanently marked `failed`.
/// `max_retries` is the **total** execution count — the initial run included.
/// A value of `3` means one original run plus two retries.
///
/// # Example
///
/// ```rust,ignore
/// use rapina::jobs::RetryPolicy;
/// use std::time::Duration;
///
/// // Exponential backoff: immediate, 1s, 4s, 16s, then fail.
/// let policy = RetryPolicy::exponential(5, Duration::from_secs(1));
/// ```
#[derive(Debug, Clone)]
pub enum RetryPolicy {
    /// Exponential backoff with jitter.
    ///
    /// Delay after attempt `n`: `base_delay * 4^(n-2) + jitter` for `n >= 2`,
    /// or immediate for `n == 1`.
    ///
    /// | Attempt | Delay (base = 1 s) |
    /// |---------|--------------------|
    /// | 1       | immediate          |
    /// | 2       | 1 s + jitter       |
    /// | 3       | 4 s + jitter       |
    /// | 4       | 16 s + jitter      |
    Exponential {
        max_retries: i32,
        base_delay: Duration,
    },
    /// Constant delay between all retries. First retry is always immediate.
    Fixed { max_retries: i32, delay: Duration },
    /// Fail permanently on the first error, no retries.
    None,
}

impl RetryPolicy {
    /// Exponential backoff with the given `max_retries` and `base_delay`.
    pub fn exponential(max_retries: i32, base_delay: Duration) -> Self {
        Self::Exponential {
            max_retries,
            base_delay,
        }
    }

    /// Constant delay between retries.
    pub fn fixed(max_retries: i32, delay: Duration) -> Self {
        Self::Fixed { max_retries, delay }
    }

    /// No retries — fail permanently on first error.
    pub fn none() -> Self {
        Self::None
    }

    /// Returns the delay to wait before the next attempt.
    ///
    /// `attempts` is the attempt count **after** the failure (already
    /// incremented). Returns [`Duration::ZERO`] for the first retry.
    pub(crate) fn backoff_delay(&self, attempts: i32, job_id: Uuid) -> Duration {
        match self {
            Self::Exponential { base_delay, .. } => {
                exponential_delay(*base_delay, attempts, job_id)
            }
            Self::Fixed { delay, .. } => {
                if attempts <= 1 {
                    Duration::ZERO
                } else {
                    *delay
                }
            }
            Self::None => Duration::ZERO,
        }
    }
}

/// Returns a deterministic offset in `[0, base)` seeded from the job's UUID.
///
/// Each job gets a unique jitter value regardless of when it was scheduled,
/// which prevents workers that fail around the same time from retrying in
/// lockstep (thundering herd).
fn jitter(base: Duration, job_id: Uuid) -> Duration {
    if base.is_zero() {
        return Duration::ZERO;
    }
    // Use the UUID as a numeric seed. Each job has a unique ID, so concurrent
    // failures produce different offsets without needing a random number generator.
    // Using wall-clock time instead would cause workers failing at the same
    // millisecond to retry in lockstep — the thundering herd we're avoiding.
    let seed = job_id.as_u128();
    let base_nanos = base.as_nanos().min(u128::from(u64::MAX)) as u64;
    // Modulo keeps the result in [0, base_nanos), so jitter is always < base.
    Duration::from_nanos((seed % u128::from(base_nanos)) as u64)
}

/// Computes the exponential backoff delay for a given attempt count.
///
/// Capped at one week so `run_at` never becomes absurdly far in the future.
fn exponential_delay(base: Duration, attempts: i32, job_id: Uuid) -> Duration {
    // First retry is always immediate — no point penalising a transient failure.
    if attempts <= 1 {
        return Duration::ZERO;
    }
    // attempts=2 → exponent=0 → multiplier=1 → base
    // attempts=3 → exponent=1 → multiplier=4 → 4×base
    // attempts=4 → exponent=2 → multiplier=16 → 16×base
    // (uses attempts-2 so the sequence starts at base, not 4×base)
    let exponent = (attempts - 2) as f64;
    let multiplier = 4.0_f64.powf(exponent);
    let secs = (base.as_secs_f64() * multiplier).min(7.0 * 24.0 * 3600.0);
    Duration::from_secs_f64(secs) + jitter(base, job_id)
}

/// Increments `attempts`, records `error`, and either reschedules the job as
/// `pending` with a backoff delay or permanently marks it `failed`.
///
/// `attempts` is the count from the DB row **before** this failure. The SQL
/// increments it. If the new count is less than `max_retries`, the job is
/// rescheduled; otherwise it is permanently failed.
pub(crate) async fn apply_failure(
    db: &impl ConnectionTrait,
    job_id: Uuid,
    error: &str,
    attempts: i32,
    max_retries: i32,
    policy: &RetryPolicy,
) -> Result<(), sea_orm::DbErr> {
    let new_attempts = attempts + 1;

    let stmt = if new_attempts < max_retries {
        let delay_secs = policy.backoff_delay(new_attempts, job_id).as_secs_f64();
        match db.get_database_backend() {
            DbBackend::Postgres => Postgres::build_retry_stmt(job_id, error, delay_secs),
            DbBackend::MySql => Mysql::build_retry_stmt(job_id, error, delay_secs),
            DbBackend::Sqlite => Sqlite::build_retry_stmt(job_id, error, delay_secs),
        }
    } else {
        match db.get_database_backend() {
            DbBackend::Postgres => Postgres::build_fail_stmt(job_id, error),
            DbBackend::MySql => Mysql::build_fail_stmt(job_id, error),
            DbBackend::Sqlite => Sqlite::build_fail_stmt(job_id, error),
        }
    };

    db.execute(stmt).await?;
    Ok(())
}

/// Marks a job as successfully completed.
pub(crate) async fn apply_success(
    db: &impl ConnectionTrait,
    job_id: Uuid,
) -> Result<(), sea_orm::DbErr> {
    let stmt = match db.get_database_backend() {
        DbBackend::Postgres => Postgres::build_success_stmt(job_id),
        DbBackend::MySql => Mysql::build_success_stmt(job_id),
        DbBackend::Sqlite => Sqlite::build_success_stmt(job_id),
    };
    db.execute(stmt).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Arbitrary UUID used as a stable seed for jitter in tests.
    const JOB_ID: Uuid = Uuid::from_u128(0xdeadbeef_cafe_babe_1234_56789abcdef0);

    #[test]
    fn exponential_attempt_1_is_immediate() {
        assert_eq!(
            exponential_delay(Duration::from_secs(1), 1, JOB_ID),
            Duration::ZERO
        );
    }

    #[test]
    fn exponential_attempt_2_equals_base() {
        // jitter is in [0, base), so delay must be in [base, 2*base)
        let base = Duration::from_secs(1);
        let delay = exponential_delay(base, 2, JOB_ID);
        assert!(delay >= base);
        assert!(delay < base * 2);
    }

    #[test]
    fn exponential_attempt_3_is_4x_base() {
        let base = Duration::from_secs(1);
        let delay = exponential_delay(base, 3, JOB_ID);
        assert!(delay >= base * 4);
        assert!(delay < base * 5);
    }

    #[test]
    fn exponential_attempt_4_is_16x_base() {
        let base = Duration::from_secs(1);
        let delay = exponential_delay(base, 4, JOB_ID);
        assert!(delay >= base * 16);
        assert!(delay < base * 17);
    }

    #[test]
    fn exponential_caps_at_one_week() {
        let base = Duration::from_secs(1);
        let one_week = Duration::from_secs(7 * 24 * 3600);
        let delay = exponential_delay(base, 50, JOB_ID);
        assert!(delay <= one_week + base); // one_week + at most one base of jitter
    }

    #[test]
    fn fixed_attempt_1_is_immediate() {
        let policy = RetryPolicy::fixed(5, Duration::from_secs(10));
        assert_eq!(policy.backoff_delay(1, JOB_ID), Duration::ZERO);
    }

    #[test]
    fn fixed_attempt_2_returns_configured_delay() {
        let d = Duration::from_secs(10);
        let policy = RetryPolicy::fixed(5, d);
        assert_eq!(policy.backoff_delay(2, JOB_ID), d);
    }

    #[test]
    fn none_always_returns_zero() {
        let policy = RetryPolicy::none();
        for attempt in 1..=5 {
            assert_eq!(policy.backoff_delay(attempt, JOB_ID), Duration::ZERO);
        }
    }

    #[test]
    fn jitter_is_within_range() {
        let base = Duration::from_secs(10);
        assert!(jitter(base, JOB_ID) < base);
    }

    #[test]
    fn jitter_zero_base_returns_zero() {
        assert_eq!(jitter(Duration::ZERO, JOB_ID), Duration::ZERO);
    }

    #[test]
    fn jitter_is_deterministic() {
        let base = Duration::from_secs(10);
        assert_eq!(jitter(base, JOB_ID), jitter(base, JOB_ID));
    }

    #[test]
    fn different_job_ids_produce_different_jitter() {
        let base = Duration::from_secs(10);
        let id1 = Uuid::from_u128(1);
        let id2 = Uuid::from_u128(2);
        assert_ne!(jitter(base, id1), jitter(base, id2));
    }
}
