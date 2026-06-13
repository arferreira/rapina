use std::collections::HashMap;
use std::path::Path;

use super::{Colorize, FieldInfo, NormalizedType, codegen};

pub(crate) mod diff;

// ---------------------------------------------------------------------------
// Intermediate representation
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct IntrospectedTable {
    name: String,
    columns: Vec<IntrospectedColumn>,
    primary_key_columns: Vec<String>,
    foreign_keys: Vec<IntrospectedForeignKey>,
}

#[derive(Debug)]
struct IntrospectedColumn {
    name: String,
    col_type: ImportType,
    is_nullable: bool,
}

#[derive(Debug)]
struct IntrospectedForeignKey {
    columns: Vec<String>,
    referenced_table: String,
    #[allow(dead_code)]
    referenced_columns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
enum ImportType {
    Standard(NormalizedType),
    #[allow(dead_code)]
    Unmappable(String),
}

impl ImportType {
    fn as_normalized(&self) -> Option<&NormalizedType> {
        if let ImportType::Standard(t) = self {
            return Some(t);
        }
        None
    }
}

impl From<NormalizedType> for ImportType {
    fn from(t: NormalizedType) -> Self {
        ImportType::Standard(t)
    }
}

// ---------------------------------------------------------------------------
// Type mappers
// ---------------------------------------------------------------------------

#[cfg(feature = "import-postgres")]
fn map_pg_type(col_type: &sea_schema::postgres::def::Type) -> ImportType {
    use sea_schema::postgres::def::Type;
    match col_type {
        Type::SmallInt | Type::Integer | Type::Serial | Type::SmallSerial => {
            ImportType::Standard(NormalizedType::I32)
        }
        Type::BigInt | Type::BigSerial => ImportType::Standard(NormalizedType::I64),
        Type::Real => ImportType::Standard(NormalizedType::F32),
        Type::DoublePrecision => ImportType::Standard(NormalizedType::F64),
        Type::Money => ImportType::Standard(NormalizedType::Decimal),
        Type::Varchar(_) | Type::Char(_) => ImportType::Standard(NormalizedType::String),
        Type::Text => ImportType::Standard(NormalizedType::Text),
        Type::Bytea => ImportType::Standard(NormalizedType::Bytes),
        Type::Boolean => ImportType::Standard(NormalizedType::Bool),
        Type::Uuid => ImportType::Standard(NormalizedType::Uuid),
        Type::TimestampWithTimeZone(_) => ImportType::Standard(NormalizedType::DateTimeUtc),
        Type::Timestamp(_) => ImportType::Standard(NormalizedType::DateTime),
        Type::Date => ImportType::Standard(NormalizedType::Date),
        Type::Time(_) => ImportType::Standard(NormalizedType::Time),
        Type::Decimal(_) | Type::Numeric(_) => ImportType::Standard(NormalizedType::Decimal),
        Type::Json | Type::JsonBinary => ImportType::Standard(NormalizedType::Json),
        other => ImportType::Unmappable(format!("{:?}", other)),
    }
}

#[cfg(feature = "import-mysql")]
fn map_mysql_type(col_type: &sea_schema::mysql::def::Type) -> ImportType {
    use sea_schema::mysql::def::Type;
    match col_type {
        Type::TinyInt(_) | Type::SmallInt(_) | Type::MediumInt(_) | Type::Int(_) => {
            ImportType::Standard(NormalizedType::I32)
        }
        Type::BigInt(_) | Type::Serial => ImportType::Standard(NormalizedType::I64),
        Type::Float(_) => ImportType::Standard(NormalizedType::F32),
        Type::Double(_) => ImportType::Standard(NormalizedType::F64),
        Type::Char(_) | Type::NChar(_) | Type::Varchar(_) | Type::NVarchar(_) => {
            ImportType::Standard(NormalizedType::String)
        }
        Type::Text(_) | Type::TinyText(_) | Type::MediumText(_) | Type::LongText(_) => {
            ImportType::Standard(NormalizedType::Text)
        }
        Type::Bool => ImportType::Standard(NormalizedType::Bool),
        Type::Timestamp(_) => ImportType::Standard(NormalizedType::DateTimeUtc),
        Type::DateTime(_) => ImportType::Standard(NormalizedType::DateTime),
        Type::Date => ImportType::Standard(NormalizedType::Date),
        Type::Decimal(_) => ImportType::Standard(NormalizedType::Decimal),
        Type::Json => ImportType::Standard(NormalizedType::Json),
        Type::Time(_) => ImportType::Standard(NormalizedType::Time),
        Type::Binary(s) if s.length == Some(16) => ImportType::Standard(NormalizedType::Uuid),
        Type::Binary(_)
        | Type::Varbinary(_)
        | Type::TinyBlob
        | Type::Blob(_)
        | Type::MediumBlob
        | Type::LongBlob => ImportType::Standard(NormalizedType::Bytes),
        other => ImportType::Unmappable(format!("{:?}", other)),
    }
}

#[cfg(feature = "import-sqlite")]
fn map_sqlite_type(col_type: &sea_schema::sea_query::ColumnType) -> ImportType {
    use sea_schema::sea_query::ColumnType;
    match col_type {
        ColumnType::TinyInteger | ColumnType::SmallInteger | ColumnType::Integer => {
            ImportType::Standard(NormalizedType::I32)
        }
        ColumnType::BigInteger => ImportType::Standard(NormalizedType::I64),
        ColumnType::Float => ImportType::Standard(NormalizedType::F32),
        ColumnType::Double => ImportType::Standard(NormalizedType::F64),
        ColumnType::String(_) | ColumnType::Char(_) => ImportType::Standard(NormalizedType::String),
        ColumnType::Text => ImportType::Standard(NormalizedType::Text),
        ColumnType::Boolean => ImportType::Standard(NormalizedType::Bool),
        ColumnType::TimestampWithTimeZone => ImportType::Standard(NormalizedType::DateTimeUtc),
        ColumnType::DateTime | ColumnType::Timestamp => {
            ImportType::Standard(NormalizedType::DateTime)
        }
        ColumnType::Date => ImportType::Standard(NormalizedType::Date),
        ColumnType::Decimal(_) | ColumnType::Money(_) => {
            ImportType::Standard(NormalizedType::Decimal)
        }
        ColumnType::Json | ColumnType::JsonBinary => ImportType::Standard(NormalizedType::Json),
        ColumnType::Uuid => ImportType::Standard(NormalizedType::Uuid),
        ColumnType::Time => ImportType::Standard(NormalizedType::Time),
        ColumnType::Blob | ColumnType::Binary(_) | ColumnType::VarBinary(_) => {
            ImportType::Standard(NormalizedType::Bytes)
        }
        ColumnType::Custom(name) => {
            let name_str = name.to_string();
            // Strip length/precision like BINARY(16) -> BINARY
            let base_name = name_str.split('(').next().unwrap_or(&name_str).trim();
            base_name
                .parse::<NormalizedType>()
                .map(ImportType::Standard)
                .unwrap_or_else(|_| ImportType::Unmappable(format!("{:?}", col_type)))
        }
        other => ImportType::Unmappable(format!("{:?}", other)),
    }
}

// ---------------------------------------------------------------------------
// NormalizedType -> FieldInfo conversion
// ---------------------------------------------------------------------------

fn normalized_to_field_info(
    col_name: &str,
    col_type: &ImportType,
    is_nullable: bool,
) -> Result<Option<FieldInfo>, String> {
    let normalized_type = match col_type {
        ImportType::Standard(t) => t.clone(),
        ImportType::Unmappable(_) => return Ok(None),
    };

    let is_primary_key = col_name == "id";

    let field = FieldInfo::new(
        col_name.to_string(),
        normalized_type,
        is_nullable,
        is_primary_key,
    )?;

    Ok(Some(field))
}

// ---------------------------------------------------------------------------
// Backend introspection
// ---------------------------------------------------------------------------

#[cfg(feature = "import-postgres")]
async fn introspect_postgres(
    url: &str,
    schema_name: &str,
) -> Result<Vec<IntrospectedTable>, String> {
    let pool = sqlx::PgPool::connect(url)
        .await
        .map_err(|e| format!("Failed to connect to Postgres: {}", e))?;

    let discovery = sea_schema::postgres::discovery::SchemaDiscovery::new(pool, schema_name);
    let schema = discovery
        .discover()
        .await
        .map_err(|e| format!("Failed to discover schema: {}", e))?;

    let mut tables = Vec::new();
    for table_def in schema.tables {
        let pk_columns: Vec<String> = table_def
            .primary_key_constraints
            .iter()
            .flat_map(|pk| pk.columns.iter().cloned())
            .collect();

        let foreign_keys: Vec<IntrospectedForeignKey> = table_def
            .reference_constraints
            .iter()
            .map(|fk| IntrospectedForeignKey {
                columns: fk.columns.clone(),
                referenced_table: fk.table.clone(),
                referenced_columns: fk.foreign_columns.clone(),
            })
            .collect();

        let columns: Vec<IntrospectedColumn> = table_def
            .columns
            .iter()
            .map(|col| IntrospectedColumn {
                name: col.name.clone(),
                col_type: map_pg_type(&col.col_type),
                is_nullable: col.not_null.is_none(),
            })
            .collect();

        tables.push(IntrospectedTable {
            name: table_def.info.name.clone(),
            columns,
            primary_key_columns: pk_columns,
            foreign_keys,
        });
    }

    Ok(tables)
}

#[cfg(feature = "import-mysql")]
async fn introspect_mysql(url: &str, schema_name: &str) -> Result<Vec<IntrospectedTable>, String> {
    let pool = sqlx::MySqlPool::connect(url)
        .await
        .map_err(|e| format!("Failed to connect to MySQL: {}", e))?;

    let discovery = sea_schema::mysql::discovery::SchemaDiscovery::new(pool, schema_name);
    let schema = discovery
        .discover()
        .await
        .map_err(|e| format!("Failed to discover schema: {}", e))?;

    let mut tables = Vec::new();
    for table_def in schema.tables {
        let pk_columns: Vec<String> = table_def
            .columns
            .iter()
            .filter(|col| col.key == sea_schema::mysql::def::ColumnKey::Primary)
            .map(|col| col.name.clone())
            .collect();

        let foreign_keys: Vec<IntrospectedForeignKey> = table_def
            .foreign_keys
            .iter()
            .map(|fk| IntrospectedForeignKey {
                columns: fk.columns.clone(),
                referenced_table: fk.referenced_table.clone(),
                referenced_columns: fk.referenced_columns.clone(),
            })
            .collect();

        let columns: Vec<IntrospectedColumn> = table_def
            .columns
            .iter()
            .map(|col| IntrospectedColumn {
                name: col.name.clone(),
                col_type: map_mysql_type(&col.col_type),
                is_nullable: col.null,
            })
            .collect();

        tables.push(IntrospectedTable {
            name: table_def.info.name.clone(),
            columns,
            primary_key_columns: pk_columns,
            foreign_keys,
        });
    }

    Ok(tables)
}

#[cfg(feature = "import-sqlite")]
async fn introspect_sqlite(url: &str) -> Result<Vec<IntrospectedTable>, String> {
    let pool = sqlx::SqlitePool::connect(url)
        .await
        .map_err(|e| format!("Failed to connect to SQLite: {}", e))?;

    let discovery = sea_schema::sqlite::discovery::SchemaDiscovery::new(pool);
    let schema: sea_schema::sqlite::def::Schema = discovery
        .discover()
        .await
        .map_err(|e| format!("Failed to discover schema: {}", e))?;

    let mut tables = Vec::new();
    for table_def in schema.tables {
        let pk_columns: Vec<String> = table_def
            .columns
            .iter()
            .filter(|col| col.primary_key)
            .map(|col| col.name.clone())
            .collect();

        // SQLite ForeignKeysInfo fields are pub(crate), so we can't
        // extract FK details from outside the crate. FK resolution
        // is skipped for SQLite imports.
        let columns: Vec<IntrospectedColumn> = table_def
            .columns
            .iter()
            .map(|col| IntrospectedColumn {
                name: col.name.clone(),
                col_type: map_sqlite_type(&col.r#type),
                is_nullable: !col.not_null,
            })
            .collect();

        tables.push(IntrospectedTable {
            name: table_def.name.clone(),
            columns,
            primary_key_columns: pk_columns,
            foreign_keys: Vec::new(),
        });
    }

    Ok(tables)
}

// ---------------------------------------------------------------------------
// Filtering and validation
// ---------------------------------------------------------------------------

const INTERNAL_TABLES: &[&str] = &[
    "seaql_migrations",
    "sqlx_migrations",
    "__diesel_schema_migrations",
];

fn filter_and_validate_tables(
    tables: Vec<IntrospectedTable>,
    table_filter: Option<&[String]>,
) -> Vec<IntrospectedTable> {
    let mut result = Vec::new();

    for table in tables {
        // Skip internal / system tables
        if INTERNAL_TABLES.contains(&table.name.as_str()) || table.name.starts_with('_') {
            continue;
        }

        // Apply user filter
        if let Some(filter) = table_filter {
            if !filter.iter().any(|f| f == &table.name) {
                continue;
            }
        }

        // Must have a primary key
        if table.primary_key_columns.is_empty() {
            eprintln!(
                "  {} table {:?} skipped -- no primary key found",
                "warn:".yellow(),
                table.name
            );
            continue;
        }

        // For single PK: must be named "id" and be i32 or Uuid
        // For composite PK: skip for now (schema! limitation)
        if table.primary_key_columns.len() > 1 {
            eprintln!(
                "  {} table {:?} skipped -- composite primary keys are not supported by schema!",
                "warn:".yellow(),
                table.name
            );
            continue;
        }

        if table.primary_key_columns.len() == 1 {
            if table.primary_key_columns[0] != "id" {
                eprintln!(
                    "  {} table {:?} skipped -- PK column is {:?} (schema! requires column named \"id\" for single PK)",
                    "warn:".yellow(),
                    table.name,
                    table.primary_key_columns[0]
                );
                continue;
            }

            if let Some(pk_col) = table.columns.iter().find(|c| c.name == "id") {
                match &pk_col.col_type {
                    ImportType::Standard(NormalizedType::I32 | NormalizedType::Uuid) => {}
                    other => {
                        eprintln!(
                            "  {} table {:?} skipped -- PK is {:?} (schema! requires i32 or Uuid)",
                            "warn:".yellow(),
                            table.name,
                            other
                        );
                        continue;
                    }
                }
            }
        }

        result.push(table);
    }

    result
}

// ---------------------------------------------------------------------------
// FK relationship resolution
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct RelationshipInfo {
    field_name: String,
    related_pascal: String,
    kind: codegen::RelationshipKind,
    fk_col: Option<String>,
}

impl From<&RelationshipInfo> for codegen::RelationshipSpec {
    fn from(r: &RelationshipInfo) -> Self {
        codegen::RelationshipSpec {
            field_name: r.field_name.clone(),
            related_pascal: r.related_pascal.clone(),
            kind: r.kind,
        }
    }
}

fn resolve_relationships(tables: &[IntrospectedTable]) -> HashMap<String, Vec<RelationshipInfo>> {
    let table_names: std::collections::HashSet<&str> =
        tables.iter().map(|t| t.name.as_str()).collect();
    let mut relationships: HashMap<String, Vec<RelationshipInfo>> = HashMap::new();

    for table in tables {
        for fk in &table.foreign_keys {
            // Only resolve if the referenced table is also being imported
            if !table_names.contains(fk.referenced_table.as_str()) {
                continue;
            }

            // Only handle single-column FKs (e.g., author_id -> users.id)
            if fk.columns.len() != 1 {
                continue;
            }

            let fk_column = &fk.columns[0];
            let field_name = fk_column.strip_suffix("_id").unwrap_or(fk_column);
            let ref_singular = codegen::singularize(&fk.referenced_table);
            let ref_pascal = codegen::to_pascal_case(&ref_singular);

            let Some(col) = table.columns.iter().find(|c| &c.name == fk_column) else {
                eprintln!(
                    "  {} table {:?}: FK column {:?} not found in column list — skipping relationship",
                    "warn:".yellow(),
                    table.name,
                    fk_column
                );
                continue;
            };
            // BelongsTo on the FK side
            relationships
                .entry(table.name.clone())
                .or_default()
                .push(RelationshipInfo {
                    field_name: field_name.to_string(),
                    related_pascal: ref_pascal.clone(),
                    kind: codegen::RelationshipKind::BelongsTo {
                        nullable: col.is_nullable,
                    },
                    fk_col: Some(fk_column.to_string()),
                });

            // HasMany on the referenced side — deduplicated so that multiple FKs
            // from the same table to the same target don't produce repeated entries.
            // O(n²) over FKs per table, but this is a one-shot CLI path, not a hot loop.
            let owner_pascal = codegen::to_pascal_case(&codegen::singularize(&table.name));
            let ref_rels = relationships
                .entry(fk.referenced_table.clone())
                .or_default();
            if !ref_rels.iter().any(|r| {
                matches!(r.kind, codegen::RelationshipKind::HasMany) && r.field_name == table.name
            }) {
                ref_rels.push(RelationshipInfo {
                    field_name: table.name.clone(),
                    related_pascal: owner_pascal,
                    kind: codegen::RelationshipKind::HasMany,
                    fk_col: None,
                });
            }
        }
    }

    relationships
}

// ---------------------------------------------------------------------------
// Timestamp detection
// ---------------------------------------------------------------------------

fn detect_timestamps(table: &IntrospectedTable) -> Option<&'static str> {
    let has_created = table.columns.iter().any(|c| c.name == "created_at");
    let has_updated = table.columns.iter().any(|c| c.name == "updated_at");

    match (has_created, has_updated) {
        (true, true) => None, // default behavior, no attribute needed
        (true, false) => Some("created_at"),
        (false, true) => Some("updated_at"),
        (false, false) => Some("none"),
    }
}

// ---------------------------------------------------------------------------
// Per-table generation
// ---------------------------------------------------------------------------

fn generate_for_table(
    table: &IntrospectedTable,
    relationships: &HashMap<String, Vec<RelationshipInfo>>,
    force: bool,
) -> Result<(), String> {
    let singular = codegen::singularize(&table.name);
    let plural = &table.name;
    let pascal = codegen::to_pascal_case(&singular);
    let pascal_plural = codegen::to_pascal_case(plural);

    let is_composite_pk = table.primary_key_columns.len() > 1;

    // For composite PK, skip only timestamps. PK columns become regular fields.
    // For single PK, skip id if it's i32 (default) and timestamps.
    // If single PK is NOT i32 (e.g. Uuid), don't skip it, so it can be marked as PK in codegen.
    let is_default_pk = !is_composite_pk
        && table
            .columns
            .iter()
            .any(|c| c.name == "id" && c.col_type == ImportType::Standard(NormalizedType::I32));

    let skip_columns: Vec<&str> = if is_composite_pk || !is_default_pk {
        vec!["created_at", "updated_at"]
    } else {
        vec!["id", "created_at", "updated_at"]
    };

    let mut fields = Vec::new();
    let mut skipped = 0;

    for col in &table.columns {
        if skip_columns.contains(&col.name.as_str()) {
            continue;
        }

        match normalized_to_field_info(&col.name, &col.col_type, col.is_nullable) {
            Ok(Some(fi)) => fields.push(fi),
            Ok(None) => {
                if let ImportType::Unmappable(ref type_name) = col.col_type {
                    eprintln!(
                        "    {} column {:?}.{:?} ({}) has no schema! equivalent -- skipped",
                        "warn:".yellow(),
                        table.name,
                        col.name,
                        type_name
                    );
                }
                skipped += 1;
            }
            Err(err) => return Err(err),
        }
    }

    let timestamps = detect_timestamps(table);

    let primary_key = if is_composite_pk {
        Some(table.primary_key_columns.clone())
    } else if !is_default_pk {
        // Special case: single PK that is NOT the default i32 "id"
        Some(table.primary_key_columns.clone())
    } else {
        None
    };

    let pk_type = table
        .columns
        .iter()
        .find(|c| c.name == "id")
        .and_then(|c| c.col_type.as_normalized())
        .cloned()
        .unwrap_or(NormalizedType::I32);

    // Resolve relationships for this table and build codegen specs.
    let table_rels = relationships
        .get(&table.name)
        .map(|v| v.as_slice())
        .unwrap_or(&[]);

    // FK columns covered by a BelongsTo are emitted as relationship fields in
    // schema!, so exclude them from the plain field list to avoid duplicates.
    let fk_cols_to_skip: std::collections::HashSet<&str> = table_rels
        .iter()
        .filter_map(|r| match r.kind {
            codegen::RelationshipKind::BelongsTo { .. } => r.fk_col.as_deref(),
            codegen::RelationshipKind::HasMany => None,
        })
        .collect();

    let entity_fields: Vec<FieldInfo> = fields
        .iter()
        .filter(|f| !fk_cols_to_skip.contains(f.name.as_str()))
        .cloned()
        .collect();

    let rel_specs: Vec<codegen::RelationshipSpec> = table_rels.iter().map(Into::into).collect();

    codegen::update_entity_file(
        &pascal,
        &entity_fields,
        timestamps,
        primary_key.as_deref(),
        force,
        &rel_specs,
    )?;
    codegen::create_migration_file(
        plural,
        &pascal_plural,
        &fields,
        false,
        primary_key.as_deref(),
    )?;
    codegen::create_feature_module(&singular, plural, &pascal, &entity_fields, &pk_type, force)?;

    println!(
        "  {} Imported table {:?} as {} ({} columns, {} skipped)",
        "✓".green(),
        table.name,
        pascal.bright_cyan(),
        fields.len(),
        skipped
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Connect to `url` and introspect its schema into the shared IR.
/// Used by both `database` (full import) and `diff` (drift report).
fn introspect(url: &str, schema_name: Option<&str>) -> Result<Vec<IntrospectedTable>, String> {
    let rt = tokio::runtime::Runtime::new()
        .map_err(|e| format!("Failed to create async runtime: {}", e))?;

    rt.block_on(async {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            #[cfg(feature = "import-postgres")]
            {
                let schema = schema_name.unwrap_or("public");
                introspect_postgres(url, schema).await
            }
            #[cfg(not(feature = "import-postgres"))]
            {
                let _ = schema_name;
                Err("Postgres support requires the import-postgres feature. \
                     Reinstall with: cargo install rapina-cli --features import-postgres"
                    .to_string())
            }
        } else if url.starts_with("mysql://") || url.starts_with("mariadb://") {
            #[cfg(feature = "import-mysql")]
            {
                let schema = schema_name
                    .or_else(|| url.rsplit('/').next())
                    .ok_or_else(|| "Could not determine database name from URL. Use --schema to specify it.".to_string())?;
                introspect_mysql(url, schema).await
            }
            #[cfg(not(feature = "import-mysql"))]
            {
                let _ = schema_name;
                Err("MySQL support requires the import-mysql feature. \
                     Reinstall with: cargo install rapina-cli --features import-mysql"
                    .to_string())
            }
        } else if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
            #[cfg(feature = "import-sqlite")]
            {
                let _ = schema_name;
                introspect_sqlite(url).await
            }
            #[cfg(not(feature = "import-sqlite"))]
            {
                let _ = schema_name;
                Err("SQLite support requires the import-sqlite feature. \
                     Reinstall with: cargo install rapina-cli --features import-sqlite"
                    .to_string())
            }
        } else {
            Err(format!(
                "Unsupported database URL scheme. Expected postgres://, mysql://, or sqlite:// -- got {:?}",
                url.split("://").next().unwrap_or("unknown")
            ))
        }
    })
}

pub fn database(
    url: &str,
    table_filter: Option<&[String]>,
    schema_name: Option<&str>,
    force: bool,
) -> Result<(), String> {
    codegen::verify_rapina_project()?;

    println!();
    println!("  {} Connecting to database...", "->".bright_cyan());

    let tables = introspect(url, schema_name)?;

    let total_discovered = tables.len();
    println!("  {} Discovered {} table(s)", "✓".green(), total_discovered);

    let tables = filter_and_validate_tables(tables, table_filter);

    println!(
        "  {} {} table(s) passed validation",
        "✓".green(),
        tables.len()
    );
    println!();

    if tables.is_empty() {
        println!("  No tables to import.");
        return Ok(());
    }

    let relationships = resolve_relationships(&tables);
    let mut imported = Vec::new();

    for table in &tables {
        let singular = codegen::singularize(&table.name);
        let pascal = codegen::to_pascal_case(&singular);
        generate_for_table(table, &relationships, force)?;
        imported.push((table.name.clone(), pascal));
    }

    // Auto-wire mod declarations into src/main.rs
    let plural_names: Vec<&str> = imported.iter().map(|(name, _)| name.as_str()).collect();
    if let Err(e) = codegen::wire_main_rs(&plural_names, Path::new(".")) {
        eprintln!("  {} Could not auto-wire main.rs: {}", "!".yellow(), e);
    }

    // Summary
    println!();
    println!(
        "  {} Imported {} table(s):",
        "Summary:".bright_yellow(),
        imported.len()
    );
    for (table_name, pascal) in &imported {
        println!("    - {} -> {}", table_name, pascal.bright_cyan());
    }

    // Next steps
    println!();
    println!("  {}:", "Next steps".bright_yellow());
    println!();
    println!("  1. Review generated files in {}", "src/".cyan());
    println!("  2. Run {} to verify", "cargo build".cyan());
    println!();

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalized_to_field_info_string_not_null() {
        let result =
            normalized_to_field_info("name", &ImportType::Standard(NormalizedType::String), false)
                .unwrap();
        let fi = result.unwrap();
        assert_eq!(fi.name, "name");
        assert_eq!(fi.normalized_type, NormalizedType::String);
        assert_eq!(
            fi.generate_column("Users"),
            ".col(ColumnDef::new(Users::Name).string().not_null())"
        );
    }

    #[test]
    fn test_normalized_to_field_info_nullable() {
        let result =
            normalized_to_field_info("bio", &ImportType::Standard(NormalizedType::String), true)
                .unwrap();
        let fi = result.unwrap();
        assert_eq!(fi.normalized_type, NormalizedType::String);
        assert_eq!(
            fi.generate_column("Profiles"),
            ".col(ColumnDef::new(Profiles::Bio).string().null())"
        );
    }

    #[test]
    fn test_normalized_to_field_info_unmappable() {
        let result =
            normalized_to_field_info("geom", &ImportType::Unmappable("geometry".into()), false)
                .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_normalized_to_field_info_all_types() {
        let cases = vec![
            (NormalizedType::String, "String", ".string().not_null()"),
            (NormalizedType::Text, "String", ".text().not_null()"),
            (NormalizedType::I32, "i32", ".integer().not_null()"),
            (NormalizedType::I64, "i64", ".big_integer().not_null()"),
            (NormalizedType::F32, "f32", ".float().not_null()"),
            (NormalizedType::F64, "f64", ".double().not_null()"),
            (
                NormalizedType::Bool,
                "bool",
                ".boolean().default(Expr::value(false)).not_null()",
            ),
            (NormalizedType::Uuid, "Uuid", ".uuid().not_null()"),
            (
                NormalizedType::DateTimeUtc,
                "DateTimeUtc",
                ".timestamp_with_time_zone().not_null()",
            ),
            (
                NormalizedType::DateTime,
                "DateTime",
                ".date_time().not_null()",
            ),
            (NormalizedType::Date, "Date", ".date().not_null()"),
            (NormalizedType::Decimal, "Decimal", ".decimal().not_null()"),
            (NormalizedType::Json, "Json", ".json().not_null()"),
            (NormalizedType::Bytes, "Vec<u8>", ".binary().not_null()"),
            (NormalizedType::Time, "Time", ".time().not_null()"),
        ];

        for (norm_type, expected_rust, expected_col_base) in cases {
            let result =
                normalized_to_field_info("x", &ImportType::Standard(norm_type.clone()), false)
                    .unwrap();
            let fi = result.unwrap();
            assert_eq!(
                fi.normalized_type.to_string(),
                expected_rust,
                "normalized_type for {:?}",
                norm_type
            );
            assert_eq!(
                fi.generate_column("Users"),
                format!(".col(ColumnDef::new(Users::X){})", expected_col_base),
                "column_method for {:?}",
                norm_type
            );
        }
    }

    #[test]
    fn test_nomalized_field_name_not_valid() {
        let fi =
            normalized_to_field_info("123bio", &ImportType::Standard(NormalizedType::Text), true);
        assert!(fi.is_err())
    }

    #[test]
    fn test_detect_timestamps_both() {
        let table = IntrospectedTable {
            name: "users".into(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                },
                IntrospectedColumn {
                    name: "created_at".into(),
                    col_type: ImportType::Standard(NormalizedType::DateTimeUtc),
                    is_nullable: false,
                },
                IntrospectedColumn {
                    name: "updated_at".into(),
                    col_type: ImportType::Standard(NormalizedType::DateTimeUtc),
                    is_nullable: false,
                },
            ],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        };
        assert_eq!(detect_timestamps(&table), None);
    }

    #[test]
    fn test_detect_timestamps_none() {
        let table = IntrospectedTable {
            name: "tokens".into(),
            columns: vec![IntrospectedColumn {
                name: "id".into(),
                col_type: ImportType::Standard(NormalizedType::I32),
                is_nullable: false,
            }],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        };
        assert_eq!(detect_timestamps(&table), Some("none"));
    }

    #[test]
    fn test_detect_timestamps_created_only() {
        let table = IntrospectedTable {
            name: "logs".into(),
            columns: vec![
                IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                },
                IntrospectedColumn {
                    name: "created_at".into(),
                    col_type: ImportType::Standard(NormalizedType::DateTimeUtc),
                    is_nullable: false,
                },
            ],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        };
        assert_eq!(detect_timestamps(&table), Some("created_at"));
    }

    #[test]
    fn test_filter_skips_internal_tables() {
        let tables = vec![
            IntrospectedTable {
                name: "seaql_migrations".into(),
                columns: vec![],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
            IntrospectedTable {
                name: "_prisma_migrations".into(),
                columns: vec![],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
        ];
        let result = filter_and_validate_tables(tables, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_skips_no_pk() {
        let tables = vec![IntrospectedTable {
            name: "events".into(),
            columns: vec![],
            primary_key_columns: vec![],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_skips_composite_pk() {
        let tables = vec![IntrospectedTable {
            name: "pivot".into(),
            columns: vec![],
            primary_key_columns: vec!["user_id".into(), "role_id".into()],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_skips_non_id_pk() {
        let tables = vec![IntrospectedTable {
            name: "events".into(),
            columns: vec![IntrospectedColumn {
                name: "event_id".into(),
                col_type: ImportType::Standard(NormalizedType::I32),
                is_nullable: false,
            }],
            primary_key_columns: vec!["event_id".into()],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_accepts_uuid_pk() {
        let tables = vec![IntrospectedTable {
            name: "events".into(),
            columns: vec![IntrospectedColumn {
                name: "id".into(),
                col_type: ImportType::Standard(NormalizedType::Uuid),
                is_nullable: false,
            }],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "events");
    }

    #[test]
    fn test_filter_skips_invalid_pk_type() {
        let tables = vec![IntrospectedTable {
            name: "events".into(),
            columns: vec![IntrospectedColumn {
                name: "id".into(),
                col_type: ImportType::Standard(NormalizedType::String), // Not i32 or Uuid
                is_nullable: false,
            }],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert!(result.is_empty());
    }

    #[test]
    fn test_filter_accepts_valid_table() {
        let tables = vec![IntrospectedTable {
            name: "users".into(),
            columns: vec![IntrospectedColumn {
                name: "id".into(),
                col_type: ImportType::Standard(NormalizedType::I32),
                is_nullable: false,
            }],
            primary_key_columns: vec!["id".into()],
            foreign_keys: vec![],
        }];
        let result = filter_and_validate_tables(tables, None);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "users");
    }

    #[test]
    fn test_filter_applies_table_filter() {
        let tables = vec![
            IntrospectedTable {
                name: "users".into(),
                columns: vec![IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                }],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
            IntrospectedTable {
                name: "posts".into(),
                columns: vec![IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                }],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
        ];
        let filter = vec!["users".to_string()];
        let result = filter_and_validate_tables(tables, Some(&filter));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "users");
    }

    #[test]
    fn test_resolve_relationships() {
        let tables = vec![
            IntrospectedTable {
                name: "users".into(),
                columns: vec![IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                }],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
            IntrospectedTable {
                name: "posts".into(),
                columns: vec![
                    IntrospectedColumn {
                        name: "id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: false,
                    },
                    IntrospectedColumn {
                        name: "user_id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: false,
                    },
                ],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![IntrospectedForeignKey {
                    columns: vec!["user_id".into()],
                    referenced_table: "users".into(),
                    referenced_columns: vec!["id".into()],
                }],
            },
        ];

        let rels = resolve_relationships(&tables);

        // posts should have a BelongsTo User
        let post_rels = rels.get("posts").unwrap();
        assert_eq!(post_rels.len(), 1);
        assert_eq!(post_rels[0].field_name, "user");
        assert_eq!(post_rels[0].related_pascal, "User");
        assert!(matches!(
            post_rels[0].kind,
            codegen::RelationshipKind::BelongsTo { nullable: false }
        ));
        assert_eq!(post_rels[0].fk_col, Some("user_id".to_string()));

        // users should have a HasMany Post
        let user_rels = rels.get("users").unwrap();
        assert_eq!(user_rels.len(), 1);
        assert_eq!(user_rels[0].field_name, "posts");
        assert_eq!(user_rels[0].related_pascal, "Post");
        assert!(matches!(
            user_rels[0].kind,
            codegen::RelationshipKind::HasMany
        ));
        assert_eq!(user_rels[0].fk_col, None);
    }

    #[test]
    fn test_resolve_relationships_nullable_fk() {
        let tables = vec![
            IntrospectedTable {
                name: "users".into(),
                columns: vec![IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                }],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
            IntrospectedTable {
                name: "comments".into(),
                columns: vec![
                    IntrospectedColumn {
                        name: "id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: false,
                    },
                    IntrospectedColumn {
                        name: "author_id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: true,
                    },
                ],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![IntrospectedForeignKey {
                    columns: vec!["author_id".into()],
                    referenced_table: "users".into(),
                    referenced_columns: vec!["id".into()],
                }],
            },
        ];

        let rels = resolve_relationships(&tables);

        let comment_rels = rels.get("comments").unwrap();
        assert_eq!(comment_rels[0].field_name, "author");
        assert!(matches!(
            comment_rels[0].kind,
            codegen::RelationshipKind::BelongsTo { nullable: true }
        ));
        assert_eq!(comment_rels[0].fk_col, Some("author_id".to_string()));
    }

    #[test]
    fn test_resolve_relationships_deduplicates_has_many() {
        // comments has two FKs to users (author_id and reviewer_id);
        // users should still get only one HasMany entry for comments.
        let tables = vec![
            IntrospectedTable {
                name: "users".into(),
                columns: vec![IntrospectedColumn {
                    name: "id".into(),
                    col_type: ImportType::Standard(NormalizedType::I32),
                    is_nullable: false,
                }],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![],
            },
            IntrospectedTable {
                name: "comments".into(),
                columns: vec![
                    IntrospectedColumn {
                        name: "id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: false,
                    },
                    IntrospectedColumn {
                        name: "author_id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: false,
                    },
                    IntrospectedColumn {
                        name: "reviewer_id".into(),
                        col_type: ImportType::Standard(NormalizedType::I32),
                        is_nullable: true,
                    },
                ],
                primary_key_columns: vec!["id".into()],
                foreign_keys: vec![
                    IntrospectedForeignKey {
                        columns: vec!["author_id".into()],
                        referenced_table: "users".into(),
                        referenced_columns: vec!["id".into()],
                    },
                    IntrospectedForeignKey {
                        columns: vec!["reviewer_id".into()],
                        referenced_table: "users".into(),
                        referenced_columns: vec!["id".into()],
                    },
                ],
            },
        ];

        let rels = resolve_relationships(&tables);

        // comments gets two BelongsTo entries (one per FK)
        let comment_rels = rels.get("comments").unwrap();
        assert_eq!(comment_rels.len(), 2);

        // users gets exactly one HasMany entry for comments, not two
        let user_rels = rels.get("users").unwrap();
        assert_eq!(user_rels.len(), 1);
        assert_eq!(user_rels[0].field_name, "comments");
    }

    #[cfg(feature = "import-postgres")]
    #[test]
    fn test_map_pg_type_integers() {
        use sea_schema::postgres::def::Type;
        assert_eq!(
            map_pg_type(&Type::SmallInt),
            ImportType::Standard(NormalizedType::I32)
        );
        assert_eq!(
            map_pg_type(&Type::Integer),
            ImportType::Standard(NormalizedType::I32)
        );
        assert_eq!(
            map_pg_type(&Type::Serial),
            ImportType::Standard(NormalizedType::I32)
        );
        assert_eq!(
            map_pg_type(&Type::BigInt),
            ImportType::Standard(NormalizedType::I64)
        );
        assert_eq!(
            map_pg_type(&Type::BigSerial),
            ImportType::Standard(NormalizedType::I64)
        );
    }

    #[cfg(feature = "import-postgres")]
    #[test]
    fn test_map_pg_type_floats() {
        use sea_schema::postgres::def::Type;
        assert_eq!(
            map_pg_type(&Type::Real),
            ImportType::Standard(NormalizedType::F32)
        );
        assert_eq!(
            map_pg_type(&Type::DoublePrecision),
            ImportType::Standard(NormalizedType::F64)
        );
    }

    #[cfg(feature = "import-postgres")]
    #[test]
    fn test_map_pg_type_strings() {
        use sea_schema::postgres::def::{StringAttr, Type};
        assert_eq!(
            map_pg_type(&Type::Varchar(StringAttr { length: None })),
            ImportType::Standard(NormalizedType::String)
        );
        assert_eq!(
            map_pg_type(&Type::Text),
            ImportType::Standard(NormalizedType::Text)
        );
    }

    #[cfg(feature = "import-postgres")]
    #[test]
    fn test_map_pg_type_special() {
        use sea_schema::postgres::def::Type;
        assert_eq!(
            map_pg_type(&Type::Boolean),
            ImportType::Standard(NormalizedType::Bool)
        );
        assert_eq!(
            map_pg_type(&Type::Uuid),
            ImportType::Standard(NormalizedType::Uuid)
        );
        assert_eq!(
            map_pg_type(&Type::Date),
            ImportType::Standard(NormalizedType::Date)
        );
        assert_eq!(
            map_pg_type(&Type::Json),
            ImportType::Standard(NormalizedType::Json)
        );
        assert_eq!(
            map_pg_type(&Type::JsonBinary),
            ImportType::Standard(NormalizedType::Json)
        );
    }

    #[cfg(feature = "import-postgres")]
    #[test]
    fn test_map_pg_type_unmappable() {
        use sea_schema::postgres::def::Type;
        assert!(matches!(
            map_pg_type(&Type::Point),
            ImportType::Unmappable(_)
        ));
    }

    #[cfg(feature = "import-sqlite")]
    #[test]
    fn test_map_sqlite_type_special() {
        use sea_schema::sea_query::ColumnType;
        assert_eq!(
            map_sqlite_type(&ColumnType::Uuid),
            ImportType::Standard(NormalizedType::Uuid)
        );
        assert_eq!(
            map_sqlite_type(&ColumnType::Integer),
            ImportType::Standard(NormalizedType::I32)
        );
    }

    #[cfg(feature = "import-mysql")]
    #[test]
    fn test_map_mysql_type_special() {
        use sea_schema::mysql::def::{NumericAttr, StringAttr, Type};
        assert_eq!(
            map_mysql_type(&Type::Binary(StringAttr {
                length: Some(16),
                ..Default::default()
            })),
            ImportType::Standard(NormalizedType::Uuid)
        );
        assert_eq!(
            map_mysql_type(&Type::Int(NumericAttr::default())),
            ImportType::Standard(NormalizedType::I32)
        );
    }
}
