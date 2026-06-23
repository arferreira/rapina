//! SeaORM migration for the `rapina_jobs` table.
//!
//! This is a framework-provided migration. Users register it in their
//! project's migration list so it runs alongside application migrations:
//!
//! ```rust,ignore
//! use rapina::jobs::create_rapina_jobs;
//!
//! rapina::migrations! {
//!     create_rapina_jobs,   // framework table — sorts first
//!     m20260315_000001_create_users,
//! }
//! ```
//!
//! The migration name uses a zero timestamp (`m00000000_000000_`) so it
//! always sorts before user migrations regardless of their dates.
//!
//! Supports PostgreSQL, MySQL 8.0+, and SQLite 3.35+.

use crate::migration::prelude::*;

/// Migration that creates the `rapina_jobs` table and its partial index.
///
/// Implements [`MigrationName`](sea_orm_migration::MigrationName) manually
/// (instead of `DeriveMigrationName`) to use a zero-timestamp prefix that
/// sorts before all user migrations.
pub struct Migration;

impl sea_orm_migration::MigrationName for Migration {
    fn name(&self) -> &str {
        "m00000000_000000_create_rapina_jobs"
    }
}

#[async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RapinaJobs::Table)
                    .col(
                        ColumnDef::new(RapinaJobs::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::Queue)
                            .string_len(255)
                            .not_null()
                            .default("default"),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::JobType)
                            .string_len(255)
                            .not_null(),
                    )
                    // `.json()` (not `json_binary()`) for MySQL/SQLite portability;
                    // Postgres loses jsonb indexing, but payload is never filtered.
                    .col(ColumnDef::new(RapinaJobs::Payload).json().not_null())
                    .col(
                        ColumnDef::new(RapinaJobs::Status)
                            .string_len(32)
                            .not_null()
                            .default("pending"),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::Attempts)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::MaxRetries)
                            .integer()
                            .not_null()
                            .default(3),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::RunAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::StartedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::LockedUntil)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(RapinaJobs::FinishedAt)
                            .timestamp_with_time_zone()
                            .null(),
                    )
                    .col(ColumnDef::new(RapinaJobs::LastError).text().null())
                    .col(ColumnDef::new(RapinaJobs::TraceId).string_len(64).null())
                    .col(
                        ColumnDef::new(RapinaJobs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for the job claiming query.
        // PostgreSQL: partial index with WHERE (pending only) — supported.
        // MySQL:      regular index — MySQL does not support partial/filtered indexes
        //             in any version, and `DROP INDEX IF EXISTS` requires 8.0.22+.
        // SQLite:     skipped — single-writer, index is unnecessary.
        // SeaORM's builder API doesn't support WHERE clauses on indexes.
        let db = manager.get_connection();
        match db.get_database_backend() {
            sea_orm::DbBackend::Postgres => {
                let sql = format!(
                    "CREATE INDEX {} \
                     ON {} ({}, {}) \
                     WHERE {} = 'pending'",
                    RapinaJobs::claimable_index(),
                    RapinaJobs::table_name(),
                    RapinaJobs::queue(),
                    RapinaJobs::run_at(),
                    RapinaJobs::status(),
                );
                db.execute_unprepared(&sql).await?;
            }
            sea_orm::DbBackend::MySql => {
                let sql = format!(
                    "CREATE INDEX {} \
                     ON {} ({}, {})",
                    RapinaJobs::claimable_index(),
                    RapinaJobs::table_name(),
                    RapinaJobs::queue(),
                    RapinaJobs::run_at(),
                );
                db.execute_unprepared(&sql).await?;
            }
            sea_orm::DbBackend::Sqlite => {
                // no index needed — SQLite is single-writer
            }
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        match db.get_database_backend() {
            sea_orm::DbBackend::Postgres => {
                let sql = format!("DROP INDEX IF EXISTS {}", RapinaJobs::claimable_index());
                db.execute_unprepared(&sql).await?;
            }
            sea_orm::DbBackend::MySql => {
                // IF EXISTS requires MySQL 8.0.22+. We skip it because `up`
                // created the index, so it exists when `down` runs.
                let sql = format!(
                    "DROP INDEX {} ON {}",
                    RapinaJobs::claimable_index(),
                    RapinaJobs::table_name(),
                );
                db.execute_unprepared(&sql).await?;
            }
            sea_orm::DbBackend::Sqlite => {
                // no index was created in `up`
            }
        }

        manager
            .drop_table(Table::drop().table(RapinaJobs::Table).to_owned())
            .await
    }
}

/// Column identifiers for the `rapina_jobs` table.
///
/// Used by SeaORM's schema builder for type-safe DDL. The `Table` variant
/// resolves to `rapina_jobs`, and each column variant resolves to its
/// snake_case equivalent (e.g., `JobType` → `job_type`).
///
/// # Referencing from raw SQL
///
/// Instead of hardcoding strings like `"rapina_jobs"`, use the accessors:
///
/// ```rust,ignore
/// use crate::jobs::RapinaJobs;
///
/// let sql = format!(
///     "UPDATE {} SET {} = 'running' WHERE {} = ?",
///     RapinaJobs::table_name(),
///     RapinaJobs::Status.to_string(),
///     RapinaJobs::Id.to_string(),
/// );
/// ```
///
/// The accessors return `&'static str` literals (not `DeriveIden::to_string()`).
/// If the iden enum is renamed, update the literals here too.
#[derive(DeriveIden)]
pub(crate) enum RapinaJobs {
    Table,
    Id,
    Queue,
    JobType,
    Payload,
    Status,
    Attempts,
    MaxRetries,
    RunAt,
    StartedAt,
    LockedUntil,
    FinishedAt,
    LastError,
    TraceId,
    CreatedAt,
}

impl RapinaJobs {
    /// The physical table name (`"rapina_jobs"`).
    ///
    /// Use this in raw SQL strings instead of hardcoding the literal, so
    /// renaming the table only requires changing this one method.
    pub fn table_name() -> &'static str {
        "rapina_jobs"
    }

    /// The partial index used by the job claiming query
    /// (`"idx_rapina_jobs_claimable"`).
    pub fn claimable_index() -> &'static str {
        "idx_rapina_jobs_claimable"
    }

    /// Column name for the primary key (`"id"`).
    pub fn id() -> &'static str {
        "id"
    }

    /// Column name for the queue discriminator (`"queue"`).
    pub fn queue() -> &'static str {
        "queue"
    }

    /// Column name for the lifecycle status (`"status"`).
    pub fn status() -> &'static str {
        "status"
    }

    /// Column name for the scheduled execution time (`"run_at"`).
    pub fn run_at() -> &'static str {
        "run_at"
    }

    /// Column name for the job type discriminator (`"job_type"`).
    pub fn job_type() -> &'static str {
        "job_type"
    }

    /// Column name for the JSON payload (`"payload"`).
    pub fn payload() -> &'static str {
        "payload"
    }

    /// Column name for the max retries counter (`"max_retries"`).
    pub fn max_retries() -> &'static str {
        "max_retries"
    }

    /// Column name for the trace identifier (`"trace_id"`).
    pub fn trace_id() -> &'static str {
        "trace_id"
    }

    /// Column name for the creation timestamp (`"created_at"`).
    pub fn created_at() -> &'static str {
        "created_at"
    }

    /// Column name for the started-at timestamp (`"started_at"`).
    pub fn started_at() -> &'static str {
        "started_at"
    }

    /// Column name for the lock expiry timestamp (`"locked_until"`).
    pub fn locked_until() -> &'static str {
        "locked_until"
    }

    /// Column name for the completion/failure timestamp (`"finished_at"`).
    pub fn finished_at() -> &'static str {
        "finished_at"
    }

    /// Column name for the attempt counter (`"attempts"`).
    pub fn attempts() -> &'static str {
        "attempts"
    }

    /// Column name for the last error message (`"last_error"`).
    pub fn last_error() -> &'static str {
        "last_error"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_name_uses_zero_timestamp() {
        let m = Migration;
        let name = sea_orm_migration::MigrationName::name(&m);
        assert_eq!(name, "m00000000_000000_create_rapina_jobs");
    }

    #[test]
    fn migration_name_sorts_before_user_migrations() {
        let framework = "m00000000_000000_create_rapina_jobs";
        let user = "m20260315_000001_create_users";
        assert!(framework < user, "framework migration must sort first");
    }

    #[test]
    fn iden_table_name() {
        assert_eq!(
            RapinaJobs::Table.to_string(),
            "rapina_jobs",
            "table name must match the raw SQL in the partial index"
        );
    }

    #[test]
    fn iden_column_names() {
        // Verify DeriveIden produces the expected snake_case names.
        // If someone reorders or renames variants, this catches it.
        let expected = [
            (RapinaJobs::Id, "id"),
            (RapinaJobs::Queue, "queue"),
            (RapinaJobs::JobType, "job_type"),
            (RapinaJobs::Payload, "payload"),
            (RapinaJobs::Status, "status"),
            (RapinaJobs::Attempts, "attempts"),
            (RapinaJobs::MaxRetries, "max_retries"),
            (RapinaJobs::RunAt, "run_at"),
            (RapinaJobs::StartedAt, "started_at"),
            (RapinaJobs::LockedUntil, "locked_until"),
            (RapinaJobs::FinishedAt, "finished_at"),
            (RapinaJobs::LastError, "last_error"),
            (RapinaJobs::TraceId, "trace_id"),
            (RapinaJobs::CreatedAt, "created_at"),
        ];
        for (iden, name) in expected {
            assert_eq!(iden.to_string(), name);
        }
    }

    #[test]
    fn iden_has_all_fourteen_columns() {
        // The RFC specifies 14 columns (excluding the Table variant).
        let columns = [
            RapinaJobs::Id,
            RapinaJobs::Queue,
            RapinaJobs::JobType,
            RapinaJobs::Payload,
            RapinaJobs::Status,
            RapinaJobs::Attempts,
            RapinaJobs::MaxRetries,
            RapinaJobs::RunAt,
            RapinaJobs::StartedAt,
            RapinaJobs::LockedUntil,
            RapinaJobs::FinishedAt,
            RapinaJobs::LastError,
            RapinaJobs::TraceId,
            RapinaJobs::CreatedAt,
        ];
        assert_eq!(columns.len(), 14);
    }
}
