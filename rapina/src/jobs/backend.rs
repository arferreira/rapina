//! Backend-specific SQL generation and claiming strategies for the jobs queue.
//!
//! Each of [`Postgres`], [`Mysql`], and [`Sqlite`] exposes the same surface:
//! building [`Statement`]s for inserts, retries, failures, successes, and
//! (except MySQL) an atomic `claim_batch` that transitions pending → running.

use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait, Value,
};
use uuid::Uuid;

use crate::jobs::JobRequest;
use crate::jobs::RapinaJobs;
use crate::jobs::model::JobRow;
use crate::jobs::worker::JobConfig;

pub struct Postgres;

impl Postgres {
    pub fn build_insert_stmt(req: JobRequest, trace_id: Option<&str>, id: Uuid) -> Statement {
        let sql = format!(
            "INSERT INTO {} \
             ({}, {}, {}, {}, {}, {}, {}, {}) \
             VALUES ($1::uuid, $2, $3, $4, $5, $6, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            RapinaJobs::table_name(),
            RapinaJobs::id(),
            RapinaJobs::job_type(),
            RapinaJobs::queue(),
            RapinaJobs::payload(),
            RapinaJobs::max_retries(),
            RapinaJobs::trace_id(),
            RapinaJobs::run_at(),
            RapinaJobs::created_at(),
        );
        Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                Value::String(Some(Box::new(id.to_string()))),
                req.job_type.into(),
                req.queue.into(),
                req.payload.into(),
                req.max_retries.into(),
                trace_id.map(ToOwned::to_owned).into(),
            ],
        )
    }

    pub async fn claim_batch(
        db: &DatabaseConnection,
        config: &JobConfig,
    ) -> Result<Vec<JobRow>, DbErr> {
        let stmt = Self::build_claim_stmt(config);
        let rows = db.query_all(stmt).await?;
        rows.iter()
            .map(|row| JobRow::from_query_result(row, ""))
            .collect()
    }

    fn build_claim_stmt(config: &JobConfig) -> Statement {
        let n = config.queues.len();
        let placeholders = (1..=n)
            .map(|i| format!("${i}"))
            .collect::<Vec<_>>()
            .join(", ");
        let batch_param = n + 1;
        let timeout_param = batch_param + 1;

        let t = RapinaJobs::table_name();
        let id = RapinaJobs::id();
        let st = RapinaJobs::status();
        let q = RapinaJobs::queue();
        let r = RapinaJobs::run_at();
        let sa = RapinaJobs::started_at();
        let lu = RapinaJobs::locked_until();

        let sql = format!(
            r#"WITH claimed AS (
                   SELECT {id} FROM {t}
                   WHERE  {st}  = 'pending'
                     AND  {q}   IN ({placeholders})
                     AND  {r} <= CURRENT_TIMESTAMP
                   ORDER  BY {r} ASC
                   LIMIT  ${batch_param}
                   FOR UPDATE SKIP LOCKED
               )
               UPDATE {t}
               SET {st}   = 'running',
                   {sa}   = CURRENT_TIMESTAMP,
                   {lu} = CURRENT_TIMESTAMP + make_interval(secs => ${timeout_param})
               FROM claimed
               WHERE {t}.{id} = claimed.{id}
               RETURNING {t}.*"#
        );

        let mut values: Vec<Value> = config
            .queues
            .iter()
            .map(|q| Value::String(Some(Box::new(q.clone()))))
            .collect();
        values.push(Value::Int(Some(config.batch_size)));
        values.push(Value::Double(Some(config.job_timeout.as_secs_f64())));

        Statement::from_sql_and_values(DbBackend::Postgres, &sql, values)
    }

    pub fn build_retry_stmt(job_id: Uuid, error: &str, delay_secs: f64) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let r = RapinaJobs::run_at();
        let lu = RapinaJobs::locked_until();
        let sa = RapinaJobs::started_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = $1,
                   {st} = 'pending',
                   {r}  = CURRENT_TIMESTAMP + make_interval(secs => $2),
                   {lu} = NULL,
                   {sa} = NULL
               WHERE {id} = $3::uuid"#
        );

        Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::Double(Some(delay_secs)),
                Value::String(Some(Box::new(job_id.to_string()))),
            ],
        )
    }

    pub fn build_fail_stmt(job_id: Uuid, error: &str) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = $1,
                   {st} = 'failed',
                   {fa} = CURRENT_TIMESTAMP
               WHERE {id} = $2::uuid"#
        );

        Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::String(Some(Box::new(job_id.to_string()))),
            ],
        )
    }

    pub fn build_success_stmt(job_id: Uuid) -> Statement {
        let t = RapinaJobs::table_name();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let lu = RapinaJobs::locked_until();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {st} = 'completed',
                   {fa} = CURRENT_TIMESTAMP,
                   {lu} = NULL
               WHERE {id} = $1::uuid"#
        );

        Statement::from_sql_and_values(
            DbBackend::Postgres,
            &sql,
            [Value::String(Some(Box::new(job_id.to_string())))],
        )
    }
}

pub struct Mysql;

impl Mysql {
    pub fn build_insert_stmt(req: JobRequest, trace_id: Option<&str>, id: Uuid) -> Statement {
        let sql = format!(
            "INSERT INTO {} \
             ({}, {}, {}, {}, {}, {}, {}, {}) \
             VALUES (?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            RapinaJobs::table_name(),
            RapinaJobs::id(),
            RapinaJobs::job_type(),
            RapinaJobs::queue(),
            RapinaJobs::payload(),
            RapinaJobs::max_retries(),
            RapinaJobs::trace_id(),
            RapinaJobs::run_at(),
            RapinaJobs::created_at(),
        );
        Statement::from_sql_and_values(
            DbBackend::MySql,
            &sql,
            [
                Value::Uuid(Some(Box::new(id))),
                req.job_type.into(),
                req.queue.into(),
                req.payload.into(),
                req.max_retries.into(),
                trace_id.map(ToOwned::to_owned).into(),
            ],
        )
    }

    pub async fn claim_batch(
        db: &DatabaseConnection,
        config: &JobConfig,
    ) -> Result<Vec<JobRow>, DbErr> {
        let txn = db.begin().await?;

        let select_sql = Self::build_claim_select(config);
        let rows = txn.query_all(select_sql).await?;
        let ids: Vec<Uuid> = rows
            .iter()
            .map(|r| {
                r.try_get::<Uuid>("", RapinaJobs::id()).or_else(|_| {
                    let s: String = r.try_get("", RapinaJobs::id())?;
                    Uuid::parse_str(&s).map_err(|e| DbErr::Type(format!("invalid uuid: {e}")))
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        if ids.is_empty() {
            txn.commit().await?;
            return Ok(vec![]);
        }

        let update_sql = Self::build_claim_update(config, &ids);
        txn.execute(update_sql).await?;

        let fetch_sql = Self::build_fetch(&ids);
        let updated = txn.query_all(fetch_sql).await?;
        txn.commit().await?;

        updated
            .iter()
            .map(|row| JobRow::from_query_result(row, ""))
            .collect()
    }

    fn build_claim_select(config: &JobConfig) -> Statement {
        let placeholders = config
            .queues
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");

        let t = RapinaJobs::table_name();
        let st = RapinaJobs::status();
        let q = RapinaJobs::queue();
        let r = RapinaJobs::run_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"SELECT {id} FROM {t}
               WHERE  {st}  = 'pending'
                 AND  {q}   IN ({placeholders})
                 AND  {r} <= CURRENT_TIMESTAMP
               ORDER  BY {r} ASC
               LIMIT  ?
               FOR UPDATE SKIP LOCKED"#
        );

        let mut values: Vec<Value> = config
            .queues
            .iter()
            .map(|q| Value::String(Some(Box::new(q.clone()))))
            .collect();
        values.push(Value::Int(Some(config.batch_size)));

        Statement::from_sql_and_values(DbBackend::MySql, &sql, values)
    }

    fn build_claim_update(config: &JobConfig, ids: &[Uuid]) -> Statement {
        let id_placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

        let t = RapinaJobs::table_name();
        let st = RapinaJobs::status();
        let sa = RapinaJobs::started_at();
        let lu = RapinaJobs::locked_until();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {st}   = 'running',
                   {sa}   = CURRENT_TIMESTAMP,
                   {lu} = CURRENT_TIMESTAMP + INTERVAL ? MICROSECOND
               WHERE {id} IN ({id_placeholders})"#
        );

        let mut values: Vec<Value> = vec![Value::BigInt(Some(
            (config.job_timeout.as_secs_f64() * 1_000_000.0) as i64,
        ))];
        values.extend(ids.iter().map(|id| Value::Uuid(Some(Box::new(*id)))));

        Statement::from_sql_and_values(DbBackend::MySql, &sql, values)
    }

    fn build_fetch(ids: &[Uuid]) -> Statement {
        let id_placeholders = ids.iter().map(|_| "?").collect::<Vec<_>>().join(", ");

        let t = RapinaJobs::table_name();
        let id = RapinaJobs::id();

        let sql = format!(r#"SELECT * FROM {t} WHERE {id} IN ({id_placeholders})"#);

        let values: Vec<Value> = ids
            .iter()
            .map(|id| Value::Uuid(Some(Box::new(*id))))
            .collect();

        Statement::from_sql_and_values(DbBackend::MySql, &sql, values)
    }

    pub fn build_retry_stmt(job_id: Uuid, error: &str, delay_secs: f64) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let r = RapinaJobs::run_at();
        let lu = RapinaJobs::locked_until();
        let sa = RapinaJobs::started_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = ?,
                   {st} = 'pending',
                   {r}  = CURRENT_TIMESTAMP + INTERVAL ? MICROSECOND,
                   {lu} = NULL,
                   {sa} = NULL
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::MySql,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::BigInt(Some((delay_secs * 1_000_000.0) as i64)),
                Value::Uuid(Some(Box::new(job_id))),
            ],
        )
    }

    pub fn build_fail_stmt(job_id: Uuid, error: &str) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = ?,
                   {st} = 'failed',
                   {fa} = CURRENT_TIMESTAMP
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::MySql,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::Uuid(Some(Box::new(job_id))),
            ],
        )
    }

    pub fn build_success_stmt(job_id: Uuid) -> Statement {
        let t = RapinaJobs::table_name();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let lu = RapinaJobs::locked_until();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {st} = 'completed',
                   {fa} = CURRENT_TIMESTAMP,
                   {lu} = NULL
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::MySql,
            &sql,
            [Value::Uuid(Some(Box::new(job_id)))],
        )
    }
}

pub struct Sqlite;

impl Sqlite {
    pub fn build_insert_stmt(req: JobRequest, trace_id: Option<&str>, id: Uuid) -> Statement {
        let sql = format!(
            "INSERT INTO {} \
             ({}, {}, {}, {}, {}, {}, {}, {}) \
             VALUES (?, ?, ?, ?, ?, ?, datetime('now'), datetime('now'))",
            RapinaJobs::table_name(),
            RapinaJobs::id(),
            RapinaJobs::job_type(),
            RapinaJobs::queue(),
            RapinaJobs::payload(),
            RapinaJobs::max_retries(),
            RapinaJobs::trace_id(),
            RapinaJobs::run_at(),
            RapinaJobs::created_at(),
        );
        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            &sql,
            [
                Value::String(Some(Box::new(id.to_string()))),
                req.job_type.into(),
                req.queue.into(),
                req.payload.into(),
                req.max_retries.into(),
                trace_id.map(ToOwned::to_owned).into(),
            ],
        )
    }

    pub async fn claim_batch(
        db: &DatabaseConnection,
        config: &JobConfig,
    ) -> Result<Vec<JobRow>, DbErr> {
        let stmt = Self::build_claim_stmt(config);
        let rows = db.query_all(stmt).await?;
        rows.iter()
            .map(|row| JobRow::from_query_result(row, ""))
            .collect()
    }

    fn build_claim_stmt(config: &JobConfig) -> Statement {
        let placeholders = config
            .queues
            .iter()
            .map(|_| "?")
            .collect::<Vec<_>>()
            .join(", ");

        let t = RapinaJobs::table_name();
        let id = RapinaJobs::id();
        let st = RapinaJobs::status();
        let sa = RapinaJobs::started_at();
        let lu = RapinaJobs::locked_until();
        let q = RapinaJobs::queue();
        let r = RapinaJobs::run_at();

        let sql = format!(
            r#"UPDATE {t}
               SET {st}   = 'running',
                   {sa}   = datetime('now'),
                   {lu} = datetime('now', '+' || ? || ' seconds')
               WHERE {id} IN (
                   SELECT {id} FROM {t}
                   WHERE {st}  = 'pending'
                     AND {q}   IN ({placeholders})
                     AND {r} <= datetime('now')
                   ORDER  BY {r} ASC
                   LIMIT  ?
               )
               RETURNING *"#,
        );

        let mut values: Vec<Value> = vec![Value::Double(Some(config.job_timeout.as_secs_f64()))];
        values.extend(
            config
                .queues
                .iter()
                .map(|q| Value::String(Some(Box::new(q.clone())))),
        );
        values.push(Value::Int(Some(config.batch_size)));

        Statement::from_sql_and_values(DbBackend::Sqlite, &sql, values)
    }

    pub fn build_retry_stmt(job_id: Uuid, error: &str, delay_secs: f64) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let r = RapinaJobs::run_at();
        let lu = RapinaJobs::locked_until();
        let sa = RapinaJobs::started_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = ?,
                   {st} = 'pending',
                   {r}  = datetime('now', '+' || ? || ' seconds'),
                   {lu} = NULL,
                   {sa} = NULL
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::Double(Some(delay_secs)),
                Value::String(Some(Box::new(job_id.to_string()))),
            ],
        )
    }

    pub fn build_fail_stmt(job_id: Uuid, error: &str) -> Statement {
        let t = RapinaJobs::table_name();
        let att = RapinaJobs::attempts();
        let le = RapinaJobs::last_error();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {att} = {att} + 1,
                   {le} = ?,
                   {st} = 'failed',
                   {fa} = datetime('now')
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            &sql,
            [
                Value::String(Some(Box::new(error.to_owned()))),
                Value::String(Some(Box::new(job_id.to_string()))),
            ],
        )
    }

    pub fn build_success_stmt(job_id: Uuid) -> Statement {
        let t = RapinaJobs::table_name();
        let st = RapinaJobs::status();
        let fa = RapinaJobs::finished_at();
        let lu = RapinaJobs::locked_until();
        let id = RapinaJobs::id();

        let sql = format!(
            r#"UPDATE {t}
               SET {st} = 'completed',
                   {fa} = datetime('now'),
                   {lu} = NULL
               WHERE {id} = ?"#
        );

        Statement::from_sql_and_values(
            DbBackend::Sqlite,
            &sql,
            [Value::String(Some(Box::new(job_id.to_string())))],
        )
    }
}
