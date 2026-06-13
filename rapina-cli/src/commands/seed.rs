//! Database seeding commands for loading, dumping, and generating seed data.

use colored::Colorize;
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseConnection, FromQueryResult, Statement,
    TransactionTrait,
};
use std::fs;
use std::path::Path;

const SEEDS_DIR: &str = "seeds";

/// An entity definition: entity name paired with its fields (name, type).
type EntitySchema = Vec<(String, Vec<(String, String)>)>;

// -- Public API --

/// Load seed data from JSON files in the `seeds/` directory into the database.
///
/// Reads each `.json` file, parses it as an array of objects, and inserts records
/// using idempotent `ON CONFLICT DO NOTHING` (Postgres/SQLite) or `INSERT IGNORE` (MySQL).
///
/// **Note:** Without `--fresh`, rows whose primary key already exists in the
/// database are silently skipped (not updated). Use `--fresh` to truncate all
/// target tables before loading. Full upsert (update-on-conflict) requires
/// per-table primary key discovery and is planned for a future iteration.
///
/// # Errors
///
/// Returns an error if the seeds directory is missing, the database is unreachable,
/// or any seed file contains invalid JSON.
pub async fn load(entity: Option<String>, fresh: bool) -> Result<(), String> {
    let seeds_path = Path::new(SEEDS_DIR);
    verify_seeds_dir(seeds_path)?;
    let conn = connect_to_db().await?;
    let seed_files = discover_seed_files(seeds_path, entity.as_deref())?;

    // Use a transaction so FK disable + inserts share the same connection
    let txn = conn
        .begin()
        .await
        .map_err(|e| format!("Failed to begin transaction: {}", e))?;

    disable_fk_checks(&txn).await?;

    if fresh {
        truncate_tables(&txn, &seed_files).await?;
    }

    for (table_name, file_path) in &seed_files {
        let content = fs::read_to_string(file_path)
            .map_err(|e| format!("Failed to read seed file '{}': {}", file_path.display(), e))?;

        let records: Vec<serde_json::Value> = serde_json::from_str(&content)
            .map_err(|e| format!("Failed to parse JSON in '{}': {}", file_path.display(), e))?;

        insert_records(&txn, table_name, &records).await?;

        println!(
            "{}: Inserted {} records into '{}'",
            "Success".green(),
            records.len(),
            table_name.cyan()
        );
    }

    enable_fk_checks(&txn).await?;

    txn.commit()
        .await
        .map_err(|e| format!("Failed to commit transaction: {}", e))?;

    Ok(())
}

/// Dump all database tables (or a specific entity) to JSON seed files.
///
/// Queries each table and writes the rows as a pretty-printed JSON array
/// to `seeds/{table_name}.json`.
///
/// # Errors
///
/// Returns an error if the database is unreachable or a table cannot be read.
pub async fn dump(entity: Option<String>) -> Result<(), String> {
    let seeds_path = Path::new(SEEDS_DIR);
    if !seeds_path.exists() {
        fs::create_dir_all(seeds_path)
            .map_err(|e| format!("Failed to create seeds directory: {}", e))?;
    }

    let conn = connect_to_db().await?;
    let tables = discover_tables(&conn, entity.as_deref()).await?;

    for table_name in &tables {
        let records = fetch_all_records(&conn, table_name).await?;
        let json = serde_json::to_string_pretty(&records)
            .map_err(|e| format!("Failed to serialize {}: {}", table_name, e))?;

        let file_path = seeds_path.join(format!("{}.json", table_name));
        fs::write(&file_path, &json)
            .map_err(|e| format!("Failed to write {}: {}", file_path.display(), e))?;

        println!(
            "{}: Dumped {} records from '{}' to '{}'",
            "Success".green(),
            records.len(),
            table_name.cyan(),
            file_path.display()
        );
    }

    Ok(())
}

/// Generate fake seed data based on `schema!` macro blocks in `src/entity.rs`.
///
/// Parses entity definitions, generates `count` records per entity with
/// type-aware fake values, and writes them to `seeds/{table_name}.json`.
///
/// # Errors
///
/// Returns an error if `src/entity.rs` is missing or contains no `schema!` blocks.
pub fn generate(count: u32, entity: Option<String>) -> Result<(), String> {
    let seeds_path = Path::new(SEEDS_DIR);
    if !seeds_path.exists() {
        fs::create_dir_all(seeds_path)
            .map_err(|e| format!("Failed to create seeds directory: {}", e))?;
    }

    let entities = parse_schema_entities()?;
    let mut rng = fastrand::Rng::new();

    for (entity_name, fields) in &entities {
        if let Some(ref filter) = entity {
            if entity_name.to_lowercase() != filter.to_lowercase() {
                continue;
            }
        }

        let records: Vec<serde_json::Value> = (0..count)
            .map(|i| generate_record(fields, i, &mut rng))
            .collect();

        let table_name = pluralize(&entity_name.to_lowercase());
        let json = serde_json::to_string_pretty(&records)
            .map_err(|e| format!("Failed to serialize: {}", e))?;

        let file_path = seeds_path.join(format!("{}.json", table_name));
        fs::write(&file_path, &json)
            .map_err(|e| format!("Failed to write {}: {}", file_path.display(), e))?;

        println!(
            "{}: Generated {} records for '{}' -> {}",
            "Success".green(),
            count,
            table_name.cyan(),
            file_path.display()
        );
    }

    Ok(())
}

// -- Identifier quoting --

/// Quote a SQL identifier using the appropriate syntax for the given database backend.
///
/// - Postgres/SQLite: `"identifier"`
/// - MySQL: `` `identifier` ``
fn quote_ident(backend: DatabaseBackend, name: &str) -> String {
    match backend {
        DatabaseBackend::MySql => format!("`{}`", name.replace('`', "``")),
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            format!("\"{}\"", name.replace('"', "\"\""))
        }
    }
}

// -- Validation --

fn verify_seeds_dir(seeds_path: &Path) -> Result<(), String> {
    if !seeds_path.exists() {
        return Err(format!(
            "Seeds directory '{}' does not exist",
            seeds_path.display()
        ));
    }
    if !seeds_path.is_dir() {
        return Err(format!(
            "Seeds path '{}' is not a directory",
            seeds_path.display()
        ));
    }
    Ok(())
}

// -- Database --

async fn connect_to_db() -> Result<DatabaseConnection, String> {
    let database_url = std::env::var("DATABASE_URL")
        .map_err(|_| "DATABASE_URL environment variable is not set".to_string())?;

    sea_orm::Database::connect(&database_url)
        .await
        .map_err(|e| format!("Failed to connect to database: {}", e))
}

async fn discover_tables(
    conn: &DatabaseConnection,
    entity: Option<&str>,
) -> Result<Vec<String>, String> {
    let sql = match conn.get_database_backend() {
        DatabaseBackend::Sqlite => "SELECT name FROM sqlite_master WHERE type='table' \
             AND name NOT LIKE 'seaql_%' AND name != 'sqlite_sequence' \
             ORDER BY name"
            .to_string(),
        DatabaseBackend::Postgres => "SELECT tablename AS name FROM pg_tables \
             WHERE schemaname = 'public' AND tablename NOT LIKE 'seaql_%' \
             ORDER BY tablename"
            .to_string(),
        DatabaseBackend::MySql => "SELECT table_name AS name FROM information_schema.tables \
             WHERE table_schema = DATABASE() AND table_name NOT LIKE 'seaql_%' \
             ORDER BY table_name"
            .to_string(),
    };

    let rows = conn
        .query_all(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .map_err(|e| format!("Failed to discover tables: {}", e))?;

    let mut tables = Vec::new();
    for row in rows {
        let name: String = row
            .try_get("", "name")
            .map_err(|e| format!("Failed to read table name: {}", e))?;
        if let Some(filter) = entity {
            if name == filter {
                tables.push(name);
            }
        } else {
            tables.push(name);
        }
    }

    Ok(tables)
}

async fn fetch_all_records(
    conn: &DatabaseConnection,
    table_name: &str,
) -> Result<Vec<serde_json::Value>, String> {
    let backend = conn.get_database_backend();
    let quoted = quote_ident(backend, table_name);

    let results: Vec<serde_json::Value> = serde_json::Value::find_by_statement(
        Statement::from_string(backend, format!("SELECT * FROM {}", quoted)),
    )
    .all(conn)
    .await
    .map_err(|e| format!("Failed to fetch records from '{}': {}", table_name, e))?;

    Ok(results)
}

async fn insert_records(
    conn: &impl ConnectionTrait,
    table_name: &str,
    records: &[serde_json::Value],
) -> Result<(), String> {
    let backend = conn.get_database_backend();
    let quoted_table = quote_ident(backend, table_name);

    for record in records {
        let obj = record
            .as_object()
            .ok_or_else(|| "Record is not a JSON object".to_string())?;

        let columns: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        let values: Vec<&serde_json::Value> = obj.values().collect();

        let quoted_columns: Vec<String> = columns.iter().map(|c| quote_ident(backend, c)).collect();

        let placeholders = build_placeholders(backend, columns.len());

        let sql = build_insert_sql(backend, &quoted_table, &quoted_columns, &placeholders);

        let sea_values: Vec<sea_orm::Value> = values.iter().map(|v| json_to_sea_value(v)).collect();

        conn.execute(Statement::from_sql_and_values(backend, &sql, sea_values))
            .await
            .map_err(|e| format!("Failed to insert record into '{}': {}", table_name, e))?;
    }
    Ok(())
}

/// Build positional parameter placeholders appropriate for the database backend.
fn build_placeholders(backend: DatabaseBackend, count: usize) -> String {
    match backend {
        DatabaseBackend::Postgres => (1..=count)
            .map(|i| format!("${}", i))
            .collect::<Vec<_>>()
            .join(", "),
        DatabaseBackend::Sqlite | DatabaseBackend::MySql => vec!["?"; count].join(", "),
    }
}

/// Build a full INSERT statement with idempotent conflict handling.
fn build_insert_sql(
    backend: DatabaseBackend,
    quoted_table: &str,
    quoted_columns: &[String],
    placeholders: &str,
) -> String {
    let cols = quoted_columns.join(", ");
    match backend {
        DatabaseBackend::MySql => {
            format!(
                "INSERT IGNORE INTO {} ({}) VALUES ({})",
                quoted_table, cols, placeholders
            )
        }
        DatabaseBackend::Postgres | DatabaseBackend::Sqlite => {
            format!(
                "INSERT INTO {} ({}) VALUES ({}) ON CONFLICT DO NOTHING",
                quoted_table, cols, placeholders
            )
        }
    }
}

async fn disable_fk_checks(conn: &impl ConnectionTrait) -> Result<(), String> {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => "PRAGMA foreign_keys = OFF",
        DatabaseBackend::Postgres => "SET session_replication_role = 'replica'",
        DatabaseBackend::MySql => "SET FOREIGN_KEY_CHECKS = 0",
    };
    conn.execute(Statement::from_string(backend, sql.to_string()))
        .await
        .map_err(|e| format!("Failed to disable foreign key checks: {}", e))?;
    Ok(())
}

async fn enable_fk_checks(conn: &impl ConnectionTrait) -> Result<(), String> {
    let backend = conn.get_database_backend();
    let sql = match backend {
        DatabaseBackend::Sqlite => "PRAGMA foreign_keys = ON",
        DatabaseBackend::Postgres => "SET session_replication_role = 'origin'",
        DatabaseBackend::MySql => "SET FOREIGN_KEY_CHECKS = 1",
    };
    conn.execute(Statement::from_string(backend, sql.to_string()))
        .await
        .map_err(|e| format!("Failed to re-enable foreign key checks: {}", e))?;
    Ok(())
}

async fn truncate_tables(
    conn: &impl ConnectionTrait,
    seed_files: &[(String, std::path::PathBuf)],
) -> Result<(), String> {
    let backend = conn.get_database_backend();

    for (table_name, _) in seed_files.iter().rev() {
        let quoted = quote_ident(backend, table_name);
        let sql = format!("DELETE FROM {}", quoted);

        conn.execute(Statement::from_string(backend, sql))
            .await
            .map_err(|e| format!("Failed to truncate table '{}': {}", table_name, e))?;

        println!(
            "{}: Truncated table '{}'",
            "Success".green(),
            table_name.cyan()
        );
    }

    Ok(())
}

// -- Seed file discovery --

fn discover_seed_files(
    seeds_path: &Path,
    entity: Option<&str>,
) -> Result<Vec<(String, std::path::PathBuf)>, String> {
    let mut seed_files = Vec::new();
    for entry in
        fs::read_dir(seeds_path).map_err(|e| format!("Failed to read seeds directory: {}", e))?
    {
        let entry = entry.map_err(|e| format!("Failed to read directory entry: {}", e))?;
        let path = entry.path();
        if path.is_file() && path.extension().and_then(|s| s.to_str()) == Some("json") {
            if let Some(file_stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(entity_name) = entity {
                    if file_stem == entity_name {
                        seed_files.push((file_stem.to_string(), path));
                    }
                } else {
                    seed_files.push((file_stem.to_string(), path));
                }
            }
        }
    }

    seed_files.sort_by(|a, b| a.0.cmp(&b.0));

    if seed_files.is_empty() {
        return Err("No seed files found.".to_string());
    }

    Ok(seed_files)
}

// -- JSON / SeaORM conversion --

fn json_to_sea_value(v: &serde_json::Value) -> sea_orm::Value {
    match v {
        serde_json::Value::Null => sea_orm::Value::String(None),
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(f) = n.as_f64() {
                f.into()
            } else {
                sea_orm::Value::String(None)
            }
        }
        serde_json::Value::String(s) => s.clone().into(),
        _ => sea_orm::Value::String(Some(Box::new(v.to_string()))),
    }
}

// -- Schema parsing (for generate) --

fn parse_schema_entities() -> Result<EntitySchema, String> {
    let entity_path = Path::new("src/entity.rs");
    if !entity_path.exists() {
        return Err("src/entity.rs not found. Run 'rapina add resource' first.".to_string());
    }

    let content =
        fs::read_to_string(entity_path).map_err(|e| format!("Failed to read entity.rs: {}", e))?;

    parse_schema_content(&content)
}

/// Parse `schema!` macro blocks into seed's `(entity, fields)` shape.
///
/// Adapter over [`super::schema_source::parse_schema_content`], which is the
/// single parser shared with `import database --diff`. Seed only needs each
/// field's name and type, so entity and field attributes are dropped here.
fn parse_schema_content(content: &str) -> Result<EntitySchema, String> {
    let entities = super::schema_source::parse_schema_content(content)?;
    Ok(entities
        .into_iter()
        .map(|e| {
            let fields = e.fields.into_iter().map(|f| (f.name, f.type_str)).collect();
            (e.name, fields)
        })
        .collect())
}

// -- Fake data generation --

/// Pluralize an English noun for table name generation.
///
/// Handles common suffixes: -s/-x/-z/-ch/-sh -> -es, consonant + y -> -ies.
/// Falls back to appending "s" for other cases.
fn pluralize(s: &str) -> String {
    if s.ends_with('s')
        || s.ends_with('x')
        || s.ends_with('z')
        || s.ends_with("ch")
        || s.ends_with("sh")
    {
        format!("{}es", s)
    } else if let Some(prefix) = s.strip_suffix('y') {
        let before_y = prefix.as_bytes().last().copied().unwrap_or(b'a');
        if matches!(before_y, b'a' | b'e' | b'i' | b'o' | b'u') {
            format!("{}s", s)
        } else {
            format!("{}ies", prefix)
        }
    } else {
        format!("{}s", s)
    }
}

fn generate_record(
    fields: &[(String, String)],
    index: u32,
    rng: &mut fastrand::Rng,
) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for (name, typ) in fields {
        obj.insert(name.clone(), generate_fake_value(name, typ, index, rng));
    }
    serde_json::Value::Object(obj)
}

fn generate_fake_value(
    field_name: &str,
    field_type: &str,
    index: u32,
    rng: &mut fastrand::Rng,
) -> serde_json::Value {
    let n: u32 = rng.u32(..);

    match field_type {
        "String" => {
            if field_name.contains("email") {
                serde_json::json!(format!("user{}@example.com", index))
            } else if field_name.contains("name") {
                let names = ["Alice", "Bob", "Carol", "Dave", "Eve", "Frank"];
                serde_json::json!(names[(n as usize) % names.len()])
            } else if field_name.contains("url") || field_name.contains("link") {
                serde_json::json!(format!("https://example.com/{}", n % 10000))
            } else {
                serde_json::json!(format!("{}_{}", field_name, index))
            }
        }
        "i32" => serde_json::json!((n % 1000) as i32),
        "i64" => serde_json::json!((n % 100000) as i64),
        "f32" | "f64" => serde_json::json!((n % 10000) as f64 / 100.0),
        "bool" => serde_json::json!(n % 2 == 0),
        "Uuid" => serde_json::json!(uuid::Uuid::new_v4().to_string()),
        "DateTime" => serde_json::json!(format!(
            "2025-01-{:02}T{:02}:{:02}:00Z",
            (n % 28) + 1,
            n % 24,
            n % 60
        )),
        "Date" => serde_json::json!(format!("2025-01-{:02}", (n % 28) + 1)),
        "Text" => serde_json::json!(format!("Sample {} text for record {}", field_name, index)),
        _ => serde_json::Value::Null,
    }
}

// -- Tests --

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_seeds_dir(dir: &Path, files: &[(&str, &str)]) {
        let seeds = dir.join("seeds");
        fs::create_dir_all(&seeds).unwrap();
        for (name, content) in files {
            fs::write(seeds.join(name), content).unwrap();
        }
    }

    // -- quote_ident --

    #[test]
    fn quote_ident_postgres_wraps_in_double_quotes() {
        assert_eq!(quote_ident(DatabaseBackend::Postgres, "users"), "\"users\"");
    }

    #[test]
    fn quote_ident_mysql_wraps_in_backticks() {
        assert_eq!(quote_ident(DatabaseBackend::MySql, "users"), "`users`");
    }

    #[test]
    fn quote_ident_escapes_embedded_quotes() {
        assert_eq!(
            quote_ident(DatabaseBackend::Postgres, "my\"table"),
            "\"my\"\"table\""
        );
        assert_eq!(
            quote_ident(DatabaseBackend::MySql, "my`table"),
            "`my``table`"
        );
    }

    // -- build_placeholders --

    #[test]
    fn build_placeholders_postgres_uses_dollar_notation() {
        assert_eq!(
            build_placeholders(DatabaseBackend::Postgres, 3),
            "$1, $2, $3"
        );
    }

    #[test]
    fn build_placeholders_sqlite_uses_question_marks() {
        assert_eq!(build_placeholders(DatabaseBackend::Sqlite, 3), "?, ?, ?");
    }

    #[test]
    fn build_placeholders_mysql_uses_question_marks() {
        assert_eq!(build_placeholders(DatabaseBackend::MySql, 2), "?, ?");
    }

    // -- build_insert_sql --

    #[test]
    fn build_insert_sql_postgres_uses_on_conflict() {
        let sql = build_insert_sql(
            DatabaseBackend::Postgres,
            "\"users\"",
            &["\"name\"".to_string(), "\"email\"".to_string()],
            "$1, $2",
        );
        assert!(sql.contains("INSERT INTO \"users\""));
        assert!(sql.contains("ON CONFLICT DO NOTHING"));
    }

    #[test]
    fn build_insert_sql_mysql_uses_insert_ignore() {
        let sql = build_insert_sql(
            DatabaseBackend::MySql,
            "`users`",
            &["`name`".to_string(), "`email`".to_string()],
            "?, ?",
        );
        assert!(sql.contains("INSERT IGNORE INTO `users`"));
        assert!(!sql.contains("ON CONFLICT"));
    }

    // -- verify_seeds_dir --

    #[test]
    fn verify_seeds_dir_returns_error_when_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("seeds");
        let result = verify_seeds_dir(&missing);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("does not exist"));
    }

    #[test]
    fn verify_seeds_dir_returns_error_when_not_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("seeds");
        fs::write(&file_path, "not a dir").unwrap();
        let result = verify_seeds_dir(&file_path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a directory"));
    }

    #[test]
    fn verify_seeds_dir_returns_ok_when_exists() {
        let tmp = tempfile::tempdir().unwrap();
        let seeds = tmp.path().join("seeds");
        fs::create_dir(&seeds).unwrap();
        assert!(verify_seeds_dir(&seeds).is_ok());
    }

    // -- discover_seed_files --

    #[test]
    fn discover_seed_files_finds_json_files() {
        let tmp = tempfile::tempdir().unwrap();
        make_seeds_dir(
            tmp.path(),
            &[
                ("users.json", "[]"),
                ("posts.json", "[]"),
                ("readme.txt", "ignore me"),
            ],
        );
        let seeds = tmp.path().join("seeds");
        let result = discover_seed_files(&seeds, None).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "posts");
        assert_eq!(result[1].0, "users");
    }

    #[test]
    fn discover_seed_files_filters_by_entity() {
        let tmp = tempfile::tempdir().unwrap();
        make_seeds_dir(tmp.path(), &[("users.json", "[]"), ("posts.json", "[]")]);
        let seeds = tmp.path().join("seeds");
        let result = discover_seed_files(&seeds, Some("users")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "users");
    }

    #[test]
    fn discover_seed_files_returns_error_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let seeds = tmp.path().join("seeds");
        fs::create_dir(&seeds).unwrap();
        let result = discover_seed_files(&seeds, None);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No seed files found"));
    }

    #[test]
    fn discover_seed_files_sorts_alphabetically() {
        let tmp = tempfile::tempdir().unwrap();
        make_seeds_dir(
            tmp.path(),
            &[
                ("zebras.json", "[]"),
                ("apples.json", "[]"),
                ("mangos.json", "[]"),
            ],
        );
        let seeds = tmp.path().join("seeds");
        let result = discover_seed_files(&seeds, None).unwrap();
        let names: Vec<&str> = result.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["apples", "mangos", "zebras"]);
    }

    // -- json_to_sea_value --

    #[test]
    fn json_to_sea_value_converts_string() {
        let v = serde_json::json!("hello");
        let result = json_to_sea_value(&v);
        assert_eq!(
            result,
            sea_orm::Value::String(Some(Box::new("hello".to_string())))
        );
    }

    #[test]
    fn json_to_sea_value_converts_integer() {
        let v = serde_json::json!(42);
        let result = json_to_sea_value(&v);
        assert_eq!(result, sea_orm::Value::BigInt(Some(42)));
    }

    #[test]
    fn json_to_sea_value_converts_bool() {
        let v = serde_json::json!(true);
        let result = json_to_sea_value(&v);
        assert_eq!(result, sea_orm::Value::Bool(Some(true)));
    }

    #[test]
    fn json_to_sea_value_converts_null() {
        let v = serde_json::json!(null);
        let result = json_to_sea_value(&v);
        assert_eq!(result, sea_orm::Value::String(None));
    }

    #[test]
    fn json_to_sea_value_converts_float() {
        let v = serde_json::json!(2.72);
        let result = json_to_sea_value(&v);
        assert_eq!(result, sea_orm::Value::Double(Some(2.72)));
    }

    #[test]
    fn json_to_sea_value_converts_array_to_string() {
        let v = serde_json::json!([1, 2, 3]);
        let result = json_to_sea_value(&v);
        assert_eq!(
            result,
            sea_orm::Value::String(Some(Box::new("[1,2,3]".to_string())))
        );
    }

    // -- pluralize --

    #[test]
    fn pluralize_regular_noun() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("post"), "posts");
    }

    #[test]
    fn pluralize_noun_ending_in_s() {
        assert_eq!(pluralize("address"), "addresses");
    }

    #[test]
    fn pluralize_noun_ending_in_x() {
        assert_eq!(pluralize("box"), "boxes");
    }

    #[test]
    fn pluralize_noun_ending_in_ch() {
        assert_eq!(pluralize("match"), "matches");
    }

    #[test]
    fn pluralize_noun_ending_in_sh() {
        assert_eq!(pluralize("wish"), "wishes");
    }

    #[test]
    fn pluralize_noun_ending_in_consonant_y() {
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("company"), "companies");
    }

    #[test]
    fn pluralize_noun_ending_in_vowel_y() {
        assert_eq!(pluralize("key"), "keys");
        assert_eq!(pluralize("day"), "days");
    }

    // -- parse_schema_content --

    #[test]
    fn parse_schema_content_extracts_entities_and_fields() {
        let input = r#"
schema! {
    User {
        name: String,
        email: String,
        age: i32,
    }
}
"#;
        let result = parse_schema_content(input).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "User");
        assert_eq!(result[0].1.len(), 3);
        assert_eq!(result[0].1[0], ("name".to_string(), "String".to_string()));
        assert_eq!(result[0].1[1], ("email".to_string(), "String".to_string()));
        assert_eq!(result[0].1[2], ("age".to_string(), "i32".to_string()));
    }

    #[test]
    fn parse_schema_content_handles_multiple_entities() {
        let input = r#"
        schema! {
            User {
                name: String,
            }
        }

        schema! {
            Post {
                title: String,
                published: bool,
            }
        }
        "#;

        let result = parse_schema_content(input).unwrap();
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "User");
        assert_eq!(result[1].0, "Post");
    }

    #[test]
    fn parse_schema_content_returns_error_when_no_schemas() {
        let input = "// just a comment\nfn main() {}";
        let result = parse_schema_content(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No schema! blocks"));
    }

    // -- generate_fake_value --

    #[test]
    fn generate_fake_value_returns_string_for_string_type() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("title", "String", 0, &mut rng);
        assert!(val.is_string());
    }

    #[test]
    fn generate_fake_value_returns_email_format_for_email_field() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("email", "String", 5, &mut rng);
        assert_eq!(val.as_str().unwrap(), "user5@example.com");
    }

    #[test]
    fn generate_fake_value_returns_number_for_i32() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("count", "i32", 0, &mut rng);
        assert!(val.is_number());
    }

    #[test]
    fn generate_fake_value_returns_bool_for_bool_type() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("active", "bool", 0, &mut rng);
        assert!(val.is_boolean());
    }

    #[test]
    fn generate_fake_value_returns_uuid_string_for_uuid_type() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("id", "Uuid", 0, &mut rng);
        let s = val.as_str().unwrap();
        assert!(uuid::Uuid::parse_str(s).is_ok(), "Invalid UUID: {}", s);
    }

    #[test]
    fn generate_fake_value_returns_null_for_unknown_type() {
        let mut rng = fastrand::Rng::new();
        let val = generate_fake_value("x", "CustomType", 0, &mut rng);
        assert!(val.is_null());
    }

    // -- generate_record --

    #[test]
    fn generate_record_produces_object_with_all_fields() {
        let fields = vec![
            ("name".to_string(), "String".to_string()),
            ("age".to_string(), "i32".to_string()),
        ];
        let mut rng = fastrand::Rng::new();
        let record = generate_record(&fields, 0, &mut rng);
        let obj = record.as_object().unwrap();
        assert!(obj.contains_key("name"));
        assert!(obj.contains_key("age"));
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use sea_orm::{ConnectionTrait, Database, DatabaseBackend, Statement};

    async fn setup_sqlite() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        conn.execute(Statement::from_string(
            DatabaseBackend::Sqlite,
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)".to_string(),
        ))
        .await
        .unwrap();
        conn
    }

    #[tokio::test]
    async fn insert_records_inserts_into_sqlite() {
        let conn = setup_sqlite().await;
        let records =
            vec![serde_json::json!({"id": 1, "name": "Alice", "email": "alice@example.com"})];
        insert_records(&conn, "users", &records).await.unwrap();

        let rows: Vec<serde_json::Value> = serde_json::Value::find_by_statement(
            Statement::from_string(DatabaseBackend::Sqlite, "SELECT * FROM users".to_string()),
        )
        .all(&conn)
        .await
        .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "Alice");
    }

    #[tokio::test]
    async fn insert_records_skips_duplicates() {
        let conn = setup_sqlite().await;
        let records = vec![serde_json::json!({"id": 1, "name": "Alice", "email": "a@b.com"})];
        insert_records(&conn, "users", &records).await.unwrap();
        insert_records(&conn, "users", &records).await.unwrap(); // should not error

        let rows: Vec<serde_json::Value> = serde_json::Value::find_by_statement(
            Statement::from_string(DatabaseBackend::Sqlite, "SELECT * FROM users".to_string()),
        )
        .all(&conn)
        .await
        .unwrap();

        assert_eq!(rows.len(), 1); // not 2
    }

    #[tokio::test]
    async fn truncate_tables_deletes_all_rows() {
        let conn = setup_sqlite().await;
        let records = vec![serde_json::json!({"id": 1, "name": "Alice", "email": "a@b.com"})];
        insert_records(&conn, "users", &records).await.unwrap();

        let seed_files = vec![("users".to_string(), std::path::PathBuf::from("users.json"))];
        truncate_tables(&conn, &seed_files).await.unwrap();

        let rows: Vec<serde_json::Value> = serde_json::Value::find_by_statement(
            Statement::from_string(DatabaseBackend::Sqlite, "SELECT * FROM users".to_string()),
        )
        .all(&conn)
        .await
        .unwrap();

        assert_eq!(rows.len(), 0);
    }
}
