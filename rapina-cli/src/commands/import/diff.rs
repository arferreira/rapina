//! Compare code entities against the live database schema (`import database --diff`).

use heck::ToSnakeCase;

use super::{Colorize, ImportType, IntrospectedColumn, IntrospectedTable};
use crate::commands::schema_source::ParsedEntity;
use crate::commands::{NormalizedType, schema_type_name};

/// Inverse of [`crate::commands::schema_type_name`]: the type string written
/// into a `schema!` block back to its [`NormalizedType`].
fn schema_type_to_normalized(s: &str) -> Option<NormalizedType> {
    Some(match s {
        "String" => NormalizedType::String,
        "Text" => NormalizedType::Text,
        "i32" => NormalizedType::I32,
        "i64" => NormalizedType::I64,
        "f32" => NormalizedType::F32,
        "f64" => NormalizedType::F64,
        "bool" => NormalizedType::Bool,
        "Uuid" => NormalizedType::Uuid,
        "DateTime" => NormalizedType::DateTimeUtc,
        "NaiveDateTime" => NormalizedType::DateTime,
        "Date" => NormalizedType::Date,
        "Decimal" => NormalizedType::Decimal,
        "Json" => NormalizedType::Json,
        "Vec<u8>" => NormalizedType::Bytes,
        "Time" => NormalizedType::Time,
        _ => return None,
    })
}

/// A table the code expects, plus columns we couldn't type (dropped from the
/// diff on both sides so they don't show up as false drift).
struct ExpectedTable {
    table: IntrospectedTable,
    unresolved: Vec<String>,
}

/// Rebuild the table a `schema!` block implies into the same IR the live
/// introspection produces, so the two are directly comparable.
fn reconstruct_table(entity: &ParsedEntity, all: &[ParsedEntity]) -> ExpectedTable {
    let mut columns = Vec::new();
    let mut unresolved = Vec::new();

    for field in &entity.fields {
        let (inner, nullable) = strip_option(&field.type_str);
        let col_name = field.column.clone().unwrap_or_else(|| field.name.clone());

        if let Some(t) = schema_type_to_normalized(inner) {
            columns.push(col(col_name, t, nullable));
        } else if inner.starts_with("Vec<") {
            // has_many: no column lives on this side
        } else if let Some(referenced) = all.iter().find(|e| e.name == inner) {
            columns.push(col(
                format!("{}_id", field.name),
                entity_pk_type(referenced),
                nullable,
            ));
        } else {
            unresolved.push(col_name);
        }
    }

    let primary_key_columns = match &entity.primary_key {
        Some(cols) => cols.clone(),
        None => vec!["id".to_string()],
    };

    // Implicit i32 PK: the macro adds `id` when no `#[primary_key]` is set and
    // no `id` field was declared. A Uuid PK arrives as an explicit
    // `#[primary_key(id)]` plus an `id: Uuid` field, so it needs no special case.
    if entity.primary_key.is_none() && !columns.iter().any(|c| c.name == "id") {
        columns.insert(0, col("id".to_string(), NormalizedType::I32, false));
    }

    if entity.has_created_at {
        columns.push(col(
            "created_at".to_string(),
            NormalizedType::DateTimeUtc,
            false,
        ));
    }
    if entity.has_updated_at {
        columns.push(col(
            "updated_at".to_string(),
            NormalizedType::DateTimeUtc,
            false,
        ));
    }

    let table = IntrospectedTable {
        name: table_name(entity),
        columns,
        primary_key_columns,
        foreign_keys: Vec::new(),
    };

    ExpectedTable { table, unresolved }
}

fn col(name: String, t: NormalizedType, is_nullable: bool) -> IntrospectedColumn {
    IntrospectedColumn {
        name,
        col_type: ImportType::Standard(t),
        is_nullable,
    }
}

/// Splits one `Option<T>` layer, returning the inner type and whether it was wrapped.
fn strip_option(type_str: &str) -> (&str, bool) {
    match type_str
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
    {
        Some(inner) => (inner.trim(), true),
        None => (type_str, false),
    }
}

/// Table name the macro would derive: `#[table_name]` if set, else snake-cased
/// entity name with a naive `s` (matches `schema!` codegen exactly).
fn table_name(entity: &ParsedEntity) -> String {
    match &entity.table_name {
        Some(name) => name.clone(),
        None => format!("{}s", entity.name.to_snake_case()),
    }
}

/// A referenced entity's PK type, used to type a `belongs_to`'s `_id` column.
/// Mirrors the macro: a single-column `#[primary_key]` resolves to that field's
/// type; a default or composite PK is i32.
fn entity_pk_type(entity: &ParsedEntity) -> NormalizedType {
    let pk_name = match entity.primary_key.as_deref() {
        Some([only]) => only.as_str(),
        _ => return NormalizedType::I32,
    };
    entity
        .fields
        .iter()
        .find(|f| f.name == pk_name)
        .and_then(|f| schema_type_to_normalized(strip_option(&f.type_str).0))
        .unwrap_or(NormalizedType::I32)
}

#[derive(Debug, PartialEq)]
enum DriftKind {
    MissingTable {
        table: String,
    },
    MissingColumn {
        table: String,
        col: String,
        expected: String,
    },
    ExtraColumn {
        table: String,
        col: String,
        db_type: String,
    },
    TypeMismatch {
        table: String,
        col: String,
        expected: String,
        found: String,
    },
    NullabilityMismatch {
        table: String,
        col: String,
        expected_nullable: bool,
    },
    PrimaryKeyMismatch {
        table: String,
        expected: Vec<String>,
        found: Vec<String>,
    },
}

#[derive(Debug, PartialEq)]
enum NoteKind {
    TextVsString { table: String, col: String },
    MysqlBoolTinyint { table: String, col: String },
    UnmanagedTable { table: String },
}

pub(crate) struct DriftReport {
    drift: Vec<DriftKind>,
    notes: Vec<NoteKind>,
}

impl DriftReport {
    pub(crate) fn has_drift(&self) -> bool {
        !self.drift.is_empty()
    }

    pub(crate) fn exit_code(&self) -> i32 {
        if self.has_drift() { 2 } else { 0 }
    }
}

/// Compare the code-side tables against the live schema. Inputs are already
/// scoped by `--tables`; internal migration tables are ignored here.
fn compute_diff(
    expected: &[ExpectedTable],
    live: &[IntrospectedTable],
    mysql: bool,
) -> DriftReport {
    let mut drift = Vec::new();
    let mut notes = Vec::new();

    for et in expected {
        let Some(live_table) = live.iter().find(|t| t.name == et.table.name) else {
            drift.push(DriftKind::MissingTable {
                table: et.table.name.clone(),
            });
            continue;
        };
        compare_columns(et, live_table, mysql, &mut drift, &mut notes);
        compare_pk(&et.table, live_table, &mut drift);
    }

    for lt in live {
        let managed = expected.iter().any(|et| et.table.name == lt.name);
        let internal =
            super::INTERNAL_TABLES.contains(&lt.name.as_str()) || lt.name.starts_with('_');
        if !managed && !internal {
            notes.push(NoteKind::UnmanagedTable {
                table: lt.name.clone(),
            });
        }
    }

    DriftReport { drift, notes }
}

fn compare_columns(
    et: &ExpectedTable,
    live: &IntrospectedTable,
    mysql: bool,
    drift: &mut Vec<DriftKind>,
    notes: &mut Vec<NoteKind>,
) {
    let table = &et.table.name;
    let skip = |name: &str| et.unresolved.iter().any(|u| u == name);

    for ec in et.table.columns.iter().filter(|c| !skip(&c.name)) {
        match live.columns.iter().find(|c| c.name == ec.name) {
            None => drift.push(DriftKind::MissingColumn {
                table: table.clone(),
                col: ec.name.clone(),
                expected: type_label(&ec.col_type),
            }),
            Some(lc) => {
                compare_type(table, ec, lc, mysql, drift, notes);
                if ec.is_nullable != lc.is_nullable {
                    drift.push(DriftKind::NullabilityMismatch {
                        table: table.clone(),
                        col: ec.name.clone(),
                        expected_nullable: ec.is_nullable,
                    });
                }
            }
        }
    }

    for lc in live.columns.iter().filter(|c| !skip(&c.name)) {
        if !et.table.columns.iter().any(|c| c.name == lc.name) {
            drift.push(DriftKind::ExtraColumn {
                table: table.clone(),
                col: lc.name.clone(),
                db_type: type_label(&lc.col_type),
            });
        }
    }
}

fn compare_type(
    table: &str,
    ec: &IntrospectedColumn,
    lc: &IntrospectedColumn,
    mysql: bool,
    drift: &mut Vec<DriftKind>,
    notes: &mut Vec<NoteKind>,
) {
    let exp = ec
        .col_type
        .as_normalized()
        .expect("expected columns are always Standard");
    let Some(found) = lc.col_type.as_normalized() else {
        drift.push(DriftKind::TypeMismatch {
            table: table.to_string(),
            col: ec.name.clone(),
            expected: type_label(&ec.col_type),
            found: type_label(&lc.col_type),
        });
        return;
    };
    if found == exp {
        return;
    }

    let text_string = matches!(
        (exp, found),
        (NormalizedType::String, NormalizedType::Text)
            | (NormalizedType::Text, NormalizedType::String)
    );
    let mysql_bool = mysql && *exp == NormalizedType::Bool && *found == NormalizedType::I32;

    if text_string {
        notes.push(NoteKind::TextVsString {
            table: table.to_string(),
            col: ec.name.clone(),
        });
    } else if mysql_bool {
        notes.push(NoteKind::MysqlBoolTinyint {
            table: table.to_string(),
            col: ec.name.clone(),
        });
    } else {
        drift.push(DriftKind::TypeMismatch {
            table: table.to_string(),
            col: ec.name.clone(),
            expected: type_label(&ec.col_type),
            found: type_label(&lc.col_type),
        });
    }
}

fn compare_pk(expected: &IntrospectedTable, live: &IntrospectedTable, drift: &mut Vec<DriftKind>) {
    let mut exp = expected.primary_key_columns.clone();
    let mut found = live.primary_key_columns.clone();
    exp.sort();
    found.sort();
    if exp != found {
        drift.push(DriftKind::PrimaryKeyMismatch {
            table: expected.name.clone(),
            expected: exp,
            found,
        });
    }
}

fn type_label(t: &ImportType) -> String {
    match t {
        ImportType::Standard(nt) => schema_type_name(nt, false),
        ImportType::Unmappable(s) => s.clone(),
    }
}

/// Read `src/entity.rs`, reconstruct the expected schema, introspect the live
/// database, and report the drift between them.
pub(crate) fn database_diff(
    url: &str,
    table_filter: Option<&[String]>,
    schema_name: Option<&str>,
) -> Result<DriftReport, String> {
    crate::commands::codegen::verify_rapina_project()?;

    let content = std::fs::read_to_string("src/entity.rs")
        .map_err(|e| format!("Failed to read src/entity.rs: {e}"))?;
    let entities = crate::commands::schema_source::parse_schema_content(&content)?;

    let mut expected: Vec<ExpectedTable> = entities
        .iter()
        .map(|e| reconstruct_table(e, &entities))
        .collect();

    for et in &expected {
        for col in &et.unresolved {
            eprintln!(
                "  {} {}.{} has no schema! type mapping -- skipped in diff",
                "warn:".yellow(),
                et.table.name,
                col
            );
        }
    }

    println!();
    println!("  {} Connecting to database...", "->".bright_cyan());
    let mut live = super::introspect(url, schema_name)?;

    if let Some(filter) = table_filter {
        expected.retain(|et| filter.iter().any(|f| f == &et.table.name));
        live.retain(|t| filter.iter().any(|f| f == &t.name));
    }

    let mysql = url.starts_with("mysql://") || url.starts_with("mariadb://");
    Ok(compute_diff(&expected, &live, mysql))
}

pub(crate) fn render(report: &DriftReport) {
    println!();
    for d in &report.drift {
        println!("  {} {}", "drift:".red().bold(), describe_drift(d));
    }
    for n in &report.notes {
        println!("  {} {}", "note:".yellow(), describe_note(n));
    }
    println!();

    if report.has_drift() {
        println!(
            "  {} {} drift item(s), {} note(s)",
            "✗".red(),
            report.drift.len(),
            report.notes.len()
        );
    } else if report.notes.is_empty() {
        println!("  {} No drift. Code and database agree.", "✓".green());
    } else {
        println!(
            "  {} No drift ({} note(s))",
            "✓".green(),
            report.notes.len()
        );
    }
}

fn describe_drift(d: &DriftKind) -> String {
    match d {
        DriftKind::MissingTable { table } => {
            format!("table {table:?} is in code but not in the database")
        }
        DriftKind::MissingColumn {
            table,
            col,
            expected,
        } => format!("{table}.{col} ({expected}) is in code but not in the database"),
        DriftKind::ExtraColumn {
            table,
            col,
            db_type,
        } => format!("{table}.{col} ({db_type}) is in the database but not in code"),
        DriftKind::TypeMismatch {
            table,
            col,
            expected,
            found,
        } => format!("{table}.{col} type differs: code {expected}, database {found}"),
        DriftKind::NullabilityMismatch {
            table,
            col,
            expected_nullable,
        } => {
            let want = if *expected_nullable {
                "nullable"
            } else {
                "not null"
            };
            format!("{table}.{col} nullability differs: code expects {want}")
        }
        DriftKind::PrimaryKeyMismatch {
            table,
            expected,
            found,
        } => format!(
            "{table} primary key differs: code [{}], database [{}]",
            expected.join(", "),
            found.join(", ")
        ),
    }
}

fn describe_note(n: &NoteKind) -> String {
    match n {
        NoteKind::TextVsString { table, col } => {
            format!("{table}.{col} is String in code, Text in the database (cosmetic)")
        }
        NoteKind::MysqlBoolTinyint { table, col } => {
            format!("{table}.{col} is bool in code, tinyint in MySQL (expected)")
        }
        NoteKind::UnmanagedTable { table } => {
            format!("table {table:?} exists in the database with no entity in code")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::schema_source::ParsedField;

    fn field(name: &str, type_str: &str) -> ParsedField {
        ParsedField {
            name: name.to_string(),
            column: None,
            type_str: type_str.to_string(),
        }
    }

    fn entity(name: &str, fields: Vec<ParsedField>) -> ParsedEntity {
        ParsedEntity {
            name: name.to_string(),
            table_name: None,
            primary_key: None,
            has_created_at: true,
            has_updated_at: true,
            fields,
        }
    }

    fn find<'a>(t: &'a IntrospectedTable, name: &str) -> &'a IntrospectedColumn {
        t.columns
            .iter()
            .find(|c| c.name == name)
            .unwrap_or_else(|| panic!("column {name:?} not found"))
    }

    #[test]
    fn reverse_map_is_the_inverse_of_schema_type_name() {
        let all = [
            NormalizedType::String,
            NormalizedType::Text,
            NormalizedType::I32,
            NormalizedType::I64,
            NormalizedType::F32,
            NormalizedType::F64,
            NormalizedType::Bool,
            NormalizedType::Uuid,
            NormalizedType::DateTimeUtc,
            NormalizedType::DateTime,
            NormalizedType::Date,
            NormalizedType::Decimal,
            NormalizedType::Json,
            NormalizedType::Bytes,
            NormalizedType::Time,
        ];
        for t in all {
            let rendered = schema_type_name(&t, false);
            assert_eq!(
                schema_type_to_normalized(&rendered),
                Some(t.clone()),
                "round trip failed for {rendered}"
            );
        }
    }

    #[test]
    fn implicit_id_is_i32_pk() {
        let e = entity("User", vec![field("name", "String")]);
        let ExpectedTable { table, unresolved } = reconstruct_table(&e, &[]);

        assert_eq!(table.name, "users");
        assert_eq!(table.primary_key_columns, vec!["id".to_string()]);
        assert_eq!(
            find(&table, "id").col_type,
            ImportType::Standard(NormalizedType::I32)
        );
        assert!(unresolved.is_empty());
    }

    #[test]
    fn explicit_uuid_id_not_duplicated() {
        let mut e = entity("User", vec![field("id", "Uuid"), field("name", "String")]);
        e.primary_key = Some(vec!["id".to_string()]);
        let table = reconstruct_table(&e, &[]).table;

        let ids: Vec<_> = table.columns.iter().filter(|c| c.name == "id").collect();
        assert_eq!(ids.len(), 1);
        assert_eq!(ids[0].col_type, ImportType::Standard(NormalizedType::Uuid));
    }

    #[test]
    fn option_field_is_nullable() {
        let e = entity("User", vec![field("email", "Option<String>")]);
        let table = reconstruct_table(&e, &[]).table;
        assert!(find(&table, "email").is_nullable);
    }

    #[test]
    fn table_name_attr_overrides() {
        let mut e = entity("Person", vec![field("name", "String")]);
        e.table_name = Some("people".to_string());
        assert_eq!(reconstruct_table(&e, &[]).table.name, "people");
    }

    #[test]
    fn belongs_to_becomes_fk_column_typed_to_referenced_pk() {
        let user = entity("User", vec![field("name", "String")]);
        let post = entity("Post", vec![field("author", "User")]);
        let table = reconstruct_table(&post, std::slice::from_ref(&user)).table;

        let fk = find(&table, "author_id");
        assert_eq!(fk.col_type, ImportType::Standard(NormalizedType::I32));
        assert!(!fk.is_nullable);
        assert!(table.columns.iter().all(|c| c.name != "author"));
    }

    #[test]
    fn belongs_to_custom_pk_types_fk_to_that_pk() {
        let mut page = entity(
            "Page",
            vec![field("slug", "String"), field("title", "String")],
        );
        page.primary_key = Some(vec!["slug".to_string()]);
        let comment = entity("Comment", vec![field("page", "Page")]);
        let table = reconstruct_table(&comment, std::slice::from_ref(&page)).table;

        let fk = find(&table, "page_id");
        assert_eq!(fk.col_type, ImportType::Standard(NormalizedType::String));
    }

    #[test]
    fn nullable_belongs_to_is_nullable_fk() {
        let user = entity("User", vec![field("name", "String")]);
        let post = entity("Post", vec![field("author", "Option<User>")]);
        let table = reconstruct_table(&post, std::slice::from_ref(&user)).table;
        assert!(find(&table, "author_id").is_nullable);
    }

    #[test]
    fn has_many_produces_no_column() {
        let post = entity("Post", vec![field("title", "String")]);
        let user = entity("User", vec![field("posts", "Vec<Post>")]);
        let table = reconstruct_table(&user, std::slice::from_ref(&post)).table;
        assert!(table.columns.iter().all(|c| c.name != "posts"));
    }

    #[test]
    fn timestamps_off_drops_columns() {
        let mut e = entity("User", vec![field("name", "String")]);
        e.has_created_at = false;
        e.has_updated_at = false;
        let table = reconstruct_table(&e, &[]).table;
        assert!(table.columns.iter().all(|c| c.name != "created_at"));
        assert!(table.columns.iter().all(|c| c.name != "updated_at"));
    }

    #[test]
    fn unresolvable_field_is_recorded_not_columned() {
        let e = entity("Order", vec![field("status", "OrderStatus")]);
        let ExpectedTable { table, unresolved } = reconstruct_table(&e, &[]);
        assert_eq!(unresolved, vec!["status".to_string()]);
        assert!(table.columns.iter().all(|c| c.name != "status"));
    }

    // -- compute_diff --

    fn itable(name: &str, columns: Vec<IntrospectedColumn>, pk: &[&str]) -> IntrospectedTable {
        IntrospectedTable {
            name: name.to_string(),
            columns,
            primary_key_columns: pk.iter().map(|s| s.to_string()).collect(),
            foreign_keys: Vec::new(),
        }
    }

    fn expected(
        name: &str,
        columns: Vec<IntrospectedColumn>,
        pk: &[&str],
        unresolved: Vec<String>,
    ) -> ExpectedTable {
        ExpectedTable {
            table: itable(name, columns, pk),
            unresolved,
        }
    }

    fn std_col(name: &str, t: NormalizedType, nullable: bool) -> IntrospectedColumn {
        col(name.to_string(), t, nullable)
    }

    #[test]
    fn identical_schemas_have_no_drift() {
        let cols = || {
            vec![
                std_col("id", NormalizedType::I32, false),
                std_col("name", NormalizedType::String, false),
            ]
        };
        let exp = vec![expected("users", cols(), &["id"], vec![])];
        let live = vec![itable("users", cols(), &["id"])];
        let report = compute_diff(&exp, &live, false);
        assert!(!report.has_drift());
        assert_eq!(report.exit_code(), 0);
        assert!(report.notes.is_empty());
    }

    #[test]
    fn missing_table_is_drift() {
        let exp = vec![expected(
            "users",
            vec![std_col("id", NormalizedType::I32, false)],
            &["id"],
            vec![],
        )];
        let report = compute_diff(&exp, &[], false);
        assert!(
            matches!(&report.drift[..], [DriftKind::MissingTable { table }] if table == "users")
        );
    }

    #[test]
    fn column_only_in_code_is_missing() {
        let exp = vec![expected(
            "users",
            vec![
                std_col("id", NormalizedType::I32, false),
                std_col("email", NormalizedType::String, false),
            ],
            &["id"],
            vec![],
        )];
        let live = vec![itable(
            "users",
            vec![std_col("id", NormalizedType::I32, false)],
            &["id"],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(
            matches!(&report.drift[..], [DriftKind::MissingColumn { col, .. }] if col == "email")
        );
    }

    #[test]
    fn column_only_in_db_is_extra() {
        let exp = vec![expected(
            "users",
            vec![std_col("id", NormalizedType::I32, false)],
            &["id"],
            vec![],
        )];
        let live = vec![itable(
            "users",
            vec![
                std_col("id", NormalizedType::I32, false),
                std_col("legacy", NormalizedType::String, false),
            ],
            &["id"],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(
            matches!(&report.drift[..], [DriftKind::ExtraColumn { col, .. }] if col == "legacy")
        );
    }

    #[test]
    fn type_difference_is_drift() {
        let exp = vec![expected(
            "users",
            vec![std_col("age", NormalizedType::I32, false)],
            &[],
            vec![],
        )];
        let live = vec![itable(
            "users",
            vec![std_col("age", NormalizedType::String, false)],
            &[],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(matches!(
            &report.drift[..],
            [DriftKind::TypeMismatch { col, expected, found, .. }]
                if col == "age" && expected == "i32" && found == "String"
        ));
    }

    #[test]
    fn unmappable_db_type_is_type_drift() {
        let exp = vec![expected(
            "places",
            vec![std_col("area", NormalizedType::String, false)],
            &[],
            vec![],
        )];
        let live = vec![itable(
            "places",
            vec![IntrospectedColumn {
                name: "area".to_string(),
                col_type: ImportType::Unmappable("Geometry".to_string()),
                is_nullable: false,
            }],
            &[],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(matches!(
            &report.drift[..],
            [DriftKind::TypeMismatch { found, .. }] if found == "Geometry"
        ));
    }

    #[test]
    fn nullability_difference_is_drift() {
        let exp = vec![expected(
            "users",
            vec![std_col("email", NormalizedType::String, false)],
            &[],
            vec![],
        )];
        let live = vec![itable(
            "users",
            vec![std_col("email", NormalizedType::String, true)],
            &[],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(matches!(
            &report.drift[..],
            [DriftKind::NullabilityMismatch { col, expected_nullable, .. }]
                if col == "email" && !expected_nullable
        ));
    }

    #[test]
    fn primary_key_compared_as_set() {
        // same columns, different order -> no drift
        let exp = vec![expected(
            "memberships",
            vec![],
            &["user_id", "team_id"],
            vec![],
        )];
        let live = vec![itable("memberships", vec![], &["team_id", "user_id"])];
        assert!(!compute_diff(&exp, &live, false).has_drift());

        // genuinely different PK -> drift
        let exp = vec![expected("users", vec![], &["id"], vec![])];
        let live = vec![itable("users", vec![], &["uuid"])];
        assert!(matches!(
            &compute_diff(&exp, &live, false).drift[..],
            [DriftKind::PrimaryKeyMismatch { .. }]
        ));
    }

    #[test]
    fn string_vs_text_is_a_note_not_drift() {
        let exp = vec![expected(
            "posts",
            vec![std_col("body", NormalizedType::String, false)],
            &[],
            vec![],
        )];
        let live = vec![itable(
            "posts",
            vec![std_col("body", NormalizedType::Text, false)],
            &[],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(!report.has_drift());
        assert!(matches!(&report.notes[..], [NoteKind::TextVsString { col, .. }] if col == "body"));
    }

    #[test]
    fn mysql_bool_tinyint_is_note_only_on_mysql() {
        let exp = || {
            vec![expected(
                "users",
                vec![std_col("active", NormalizedType::Bool, false)],
                &[],
                vec![],
            )]
        };
        let live = || {
            vec![itable(
                "users",
                vec![std_col("active", NormalizedType::I32, false)],
                &[],
            )]
        };

        let on_mysql = compute_diff(&exp(), &live(), true);
        assert!(!on_mysql.has_drift());
        assert!(matches!(
            &on_mysql.notes[..],
            [NoteKind::MysqlBoolTinyint { .. }]
        ));

        let off_mysql = compute_diff(&exp(), &live(), false);
        assert!(matches!(
            &off_mysql.drift[..],
            [DriftKind::TypeMismatch { .. }]
        ));
    }

    #[test]
    fn db_table_without_entity_is_unmanaged_note() {
        let exp = vec![expected(
            "users",
            vec![std_col("id", NormalizedType::I32, false)],
            &["id"],
            vec![],
        )];
        let live = vec![
            itable(
                "users",
                vec![std_col("id", NormalizedType::I32, false)],
                &["id"],
            ),
            itable(
                "audit_log",
                vec![std_col("id", NormalizedType::I32, false)],
                &["id"],
            ),
            itable("seaql_migrations", vec![], &[]),
        ];
        let report = compute_diff(&exp, &live, false);
        assert!(!report.has_drift());
        // audit_log is unmanaged; seaql_migrations is internal and ignored
        assert!(
            matches!(&report.notes[..], [NoteKind::UnmanagedTable { table }] if table == "audit_log")
        );
    }

    #[test]
    fn unresolved_columns_are_dropped_from_both_sides() {
        let exp = vec![expected(
            "orders",
            vec![std_col("id", NormalizedType::I32, false)],
            &["id"],
            vec!["status".to_string()],
        )];
        // DB has the status column we couldn't type; it must not show as extra
        let live = vec![itable(
            "orders",
            vec![
                std_col("id", NormalizedType::I32, false),
                std_col("status", NormalizedType::String, false),
            ],
            &["id"],
        )];
        let report = compute_diff(&exp, &live, false);
        assert!(!report.has_drift());
    }
}
