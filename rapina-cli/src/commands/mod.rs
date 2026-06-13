//! Core command layer for `rapina-cli`.
//!
//! This module has two responsibilities:
//! 1. Expose each command module (`add`, `migrate`, `routes`, etc.).
//! 2. Provide shared command-domain primitives used across generators/importers.
//!
//! The shared primitives here are intentionally generic and CLI-oriented:
//! - [`NormalizedType`]: canonical field type vocabulary accepted by the CLI.
//! - [`FieldInfo`]: normalized description of a user/database field.
//! - [`ValidationContext`]: naming rules for resources and fields.
//!
//! Important boundary: database-import-specific edge cases must stay in
//! `commands/import.rs` (for example unmappable vendor-specific types), while
//! this module only keeps types that are valid across the command surface.

pub mod add;
pub mod agents;
pub(crate) mod codegen;
pub mod dev;
pub mod doctor;
#[cfg(feature = "import")]
pub mod import;
pub mod jobs;
pub mod llms;
pub mod migrate;
pub mod new;
pub mod openapi;
pub(crate) mod relationships;
pub mod routes;

#[cfg(feature = "seed")]
pub mod seed;

#[cfg(any(feature = "seed", feature = "import"))]
pub(crate) mod schema_source;

pub mod templates;
pub mod test;

#[cfg(feature = "import-openapi")]
pub mod import_openapi;

use colored::Colorize;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NormalizedType {
    String,
    Text,
    I32,
    I64,
    F32,
    F64,
    Bool,
    Uuid,
    DateTimeUtc,
    DateTime,
    Date,
    Decimal,
    Json,
    Bytes,
    Time,
}

/// Converts user-facing type aliases into the CLI canonical type system.
///
/// This parser is used by command inputs (e.g. schema definitions from flags)
/// and should remain stable and forgiving with common aliases.
impl std::str::FromStr for NormalizedType {
    type Err = String;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_lowercase().as_str() {
            "string" | "str" => Ok(NormalizedType::String),
            "text" => Ok(NormalizedType::Text),
            "i32" | "integer" | "int" => Ok(NormalizedType::I32),
            "i64" | "bigint" => Ok(NormalizedType::I64),
            "f32" | "float" => Ok(NormalizedType::F32),
            "f64" | "double" => Ok(NormalizedType::F64),
            "bool" | "boolean" => Ok(NormalizedType::Bool),
            "uuid" => Ok(NormalizedType::Uuid),
            "datetimeutc" | "datetime" | "timestamptz" => Ok(NormalizedType::DateTimeUtc),
            "naivedatetime" | "timestamp" => Ok(NormalizedType::DateTime),
            "date" => Ok(NormalizedType::Date),
            "decimal" | "numeric" | "money" => Ok(NormalizedType::Decimal),
            "json" | "jsonb" => Ok(NormalizedType::Json),
            "bytes" | "blob" | "binary" | "bytea" | "varbinary" => Ok(NormalizedType::Bytes),
            "time" => Ok(NormalizedType::Time),
            _ => Err(format!("Unknown field type '{}'", value)),
        }
    }
}

/// Human-readable canonical type label used by CLI/codegen internals.
///
/// This is **not** the same mapping used by generated schema Rust types
/// (see [`schema_type_name`]). For example:
/// - `DateTimeUtc` -> `"DateTimeUtc"` here
/// - `DateTimeUtc` -> `"DateTime"` in [`schema_type_name`]
impl std::fmt::Display for NormalizedType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            NormalizedType::String | NormalizedType::Text => "String",
            NormalizedType::I32 => "i32",
            NormalizedType::I64 => "i64",
            NormalizedType::F32 => "f32",
            NormalizedType::F64 => "f64",
            NormalizedType::Bool => "bool",
            NormalizedType::Uuid => "Uuid",
            NormalizedType::DateTimeUtc => "DateTimeUtc",
            NormalizedType::DateTime => "DateTime",
            NormalizedType::Date => "Date",
            NormalizedType::Decimal => "Decimal",
            NormalizedType::Json => "Json",
            NormalizedType::Bytes => "Vec<u8>",
            NormalizedType::Time => "Time",
        };
        write!(f, "{}", s)
    }
}

impl NormalizedType {
    /// Returns SeaORM import names for types that require explicit imports.
    pub(crate) fn sea_orm_import_name(&self) -> Option<&'static str> {
        match self {
            NormalizedType::DateTimeUtc => Some("DateTimeUtc"),
            NormalizedType::DateTime => Some("DateTime"),
            NormalizedType::Date => Some("Date"),
            NormalizedType::Json => Some("Json"),
            NormalizedType::Time => Some("Time"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct FieldInfo {
    /// Source field name in snake_case (e.g. `created_at`).
    pub name: String,
    /// Column enum variant identifier in PascalCase (e.g. `CreatedAt`).
    pub ident: String,
    /// Canonical type resolved by CLI parsing/introspection.
    pub normalized_type: NormalizedType,
    /// Whether generated schema/migration should mark the column nullable.
    pub nullable: bool,
    /// Whether this field is part of the primary key in generated artifacts.
    pub is_primary_key: bool,
    /// True when this field belongs to a composite (multi-column) primary key.
    /// When set, `.auto_increment()` is suppressed — composite PK columns must
    /// not be auto-increment (invalid SQL on all major databases).
    pub is_composite_pk_member: bool,
}

/// Parses `name:type` CLI field arguments into a validated [`FieldInfo`].
impl std::str::FromStr for FieldInfo {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (name, type_str) = value
            .split_once(':')
            .ok_or_else(|| format!("Invalid field format: '{}'. Expected 'name:type'", value))?;

        ValidationContext::Field.validate(name)?;

        let ident = codegen::to_pascal_case(name);
        let is_primary_key = name == "id";
        let normalized_type = type_str.parse::<NormalizedType>()?;
        let nullable = false;

        Ok(FieldInfo {
            name: name.to_string(),
            ident,
            normalized_type,
            nullable,
            is_primary_key,
            is_composite_pk_member: false,
        })
    }
}

impl FieldInfo {
    #[cfg(feature = "import")]
    pub fn new(
        name: String,
        normalized_type: NormalizedType,
        nullable: bool,
        is_primary_key: bool,
    ) -> Result<Self, String> {
        ValidationContext::Field.validate(&name)?;
        let ident = codegen::to_pascal_case(&name);
        Ok(Self {
            name,
            ident,
            normalized_type,
            nullable,
            is_primary_key,
            is_composite_pk_member: false,
        })
    }

    fn generate_column(&self, table_pascal_plural: &str) -> String {
        let base = match self.normalized_type {
            NormalizedType::String => ".string()",
            NormalizedType::Text => ".text()",
            NormalizedType::I32 => ".integer()",
            NormalizedType::I64 => ".big_integer()",
            NormalizedType::F32 => ".float()",
            NormalizedType::F64 => ".double()",
            NormalizedType::Bool => ".boolean().default(Expr::value(false))",
            NormalizedType::Uuid => ".uuid()",
            NormalizedType::DateTimeUtc => ".timestamp_with_time_zone()",
            NormalizedType::DateTime => ".date_time()",
            NormalizedType::Date => ".date()",
            NormalizedType::Decimal => ".decimal()",
            NormalizedType::Json => ".json()",
            NormalizedType::Bytes => ".binary()",
            NormalizedType::Time => ".time()",
        };

        let mut method = base.to_string();

        if self.nullable || base.is_empty() {
            method.push_str(".null()");
        } else {
            method.push_str(".not_null()");
        }

        if self.is_primary_key {
            method.push_str(".primary_key()");
            if !self.is_composite_pk_member
                && matches!(
                    self.normalized_type,
                    NormalizedType::I32 | NormalizedType::I64
                )
            {
                method.push_str(".auto_increment()");
            }
        }

        format!(
            ".col(ColumnDef::new({}::{}){})",
            table_pascal_plural, self.ident, method
        )
    }
}

pub(crate) enum ValidationContext {
    Field,
    Resource,
}

impl ValidationContext {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Field => "Field name",
            Self::Resource => "Resource name",
        }
    }

    /// Validates CLI identifiers for resources/fields.
    ///
    /// Rules are intentionally strict to guarantee generated Rust identifiers,
    /// module names, and schema symbols remain deterministic and compile-safe.
    pub(crate) fn validate(&self, name: &str) -> Result<(), String> {
        let ctx_prefix = self.as_str();

        if name.is_empty() {
            return Err(format!("{} cannot be empty", ctx_prefix));
        }

        if name.starts_with(|c: char| c.is_ascii_digit()) {
            return Err(format!("{} cannot start with a digit", ctx_prefix));
        }

        if let Some(c) = name
            .chars()
            .find(|&c| !c.is_ascii_lowercase() && !c.is_ascii_digit() && c != '_')
        {
            return Err(format!(
                "{} must be lowercase alphanumeric with underscores, got '{}'",
                ctx_prefix, c
            ));
        }

        if name.starts_with('_') || name.ends_with('_') {
            return Err(format!(
                "{} cannot start or end with underscore",
                ctx_prefix
            ));
        }

        let reserved = [
            "self", "super", "crate", "mod", "type", "fn", "struct", "enum", "impl",
        ];
        if reserved.contains(&name) {
            return Err(format!(
                "'{}' is a reserved Rust keyword and cannot be used as a {}",
                name,
                ctx_prefix.to_lowercase()
            ));
        }

        Ok(())
    }
}

/// Returns the concrete Rust type name used in generated `schema!` fields.
///
/// This mapping is schema-code specific and can differ from [`Display`] for
/// [`NormalizedType`]. In particular:
/// - `NormalizedType::DateTimeUtc` maps to `"DateTime"`
/// - `NormalizedType::DateTime` maps to `"NaiveDateTime"`
pub(crate) fn schema_type_name(normalized: &NormalizedType, nullable: bool) -> String {
    let s = match normalized {
        NormalizedType::String => "String".to_string(),
        NormalizedType::Text => "Text".to_string(),
        NormalizedType::I32 => "i32".to_string(),
        NormalizedType::I64 => "i64".to_string(),
        NormalizedType::F32 => "f32".to_string(),
        NormalizedType::F64 => "f64".to_string(),
        NormalizedType::Bool => "bool".to_string(),
        NormalizedType::Uuid => "Uuid".to_string(),
        NormalizedType::DateTimeUtc => "DateTime".to_string(),
        NormalizedType::DateTime => "NaiveDateTime".to_string(),
        NormalizedType::Date => "Date".to_string(),
        NormalizedType::Decimal => "Decimal".to_string(),
        NormalizedType::Json => "Json".to_string(),
        NormalizedType::Bytes => "Vec<u8>".to_string(),
        NormalizedType::Time => "Time".to_string(),
    };

    if nullable { format!("Option<{s}>") } else { s }
}

/// Verify that we're in a valid Rapina project directory.
pub fn verify_rapina_project() -> Result<toml::Value, String> {
    let cargo_toml = std::path::Path::new("Cargo.toml");
    if !cargo_toml.exists() {
        return Err("No Cargo.toml found. Are you in a Rust project directory?".to_string());
    }

    let content = std::fs::read_to_string(cargo_toml)
        .map_err(|e| format!("Failed to read Cargo.toml: {}", e))?;

    let parsed: toml::Value =
        toml::from_str(&content).map_err(|e| format!("Failed to parse Cargo.toml: {}", e))?;

    // Check for rapina in dependencies
    let has_rapina = parsed
        .get("dependencies")
        .and_then(|deps| deps.get("rapina"))
        .is_some();

    if !has_rapina {
        return Err(
            "This doesn't appear to be a Rapina project (no rapina dependency found)".to_string(),
        );
    }

    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::{FieldInfo, NormalizedType, schema_type_name};
    use crate::commands::ValidationContext;

    #[test]
    fn test_parse_field_valid() {
        let field: FieldInfo = "name:string".parse().unwrap();
        assert_eq!(field.name, "name");
        assert_eq!(field.normalized_type, NormalizedType::String);
        assert_eq!(
            field.generate_column("Post"),
            ".col(ColumnDef::new(Post::Name).string().not_null())".to_string()
        );
        assert!(!field.nullable);
    }

    #[test]
    fn test_parse_field_invalid() {
        let field: Result<FieldInfo, String> = "name".parse();
        assert!(field.is_err());
    }

    #[test]
    fn test_parse_field_invalid_type() {
        let field: Result<FieldInfo, String> = "name:invalid".parse();
        assert!(field.is_err());
    }

    #[test]
    fn test_parse_field_invalid_name() {
        let field: Result<FieldInfo, String> = "_name:string".parse();
        assert!(field.is_err());
    }

    #[test]
    fn test_parse_field_invalid_name_empty() {
        let field: Result<FieldInfo, String> = ":string".parse();
        assert!(field.is_err());
    }

    #[test]
    fn test_parse_field_valid_nullable() {
        let field: FieldInfo = "name:string".parse().unwrap();
        assert_eq!(field.name, "name");
        assert_eq!(field.ident, "Name");
        assert_eq!(field.normalized_type, NormalizedType::String);
        assert_eq!(
            field.generate_column("Post"),
            ".col(ColumnDef::new(Post::Name).string().not_null())".to_string()
        );
        assert!(!field.nullable);
    }

    #[test]
    fn test_validate_resource_name_valid() {
        assert!(ValidationContext::Resource.validate("user").is_ok());
        assert!(ValidationContext::Resource.validate("blog_post").is_ok());
        assert!(ValidationContext::Resource.validate("item123").is_ok());
    }

    #[test]
    fn test_validate_resource_name_invalid() {
        assert!(ValidationContext::Resource.validate("").is_err());
        assert!(ValidationContext::Resource.validate("User").is_err());
        assert!(ValidationContext::Resource.validate("_user").is_err());
        assert!(ValidationContext::Resource.validate("user_").is_err());
        assert!(ValidationContext::Resource.validate("self").is_err());
        assert!(ValidationContext::Resource.validate("user-name").is_err());
    }

    #[test]
    fn test_validate_field_name_valid() {
        assert!(ValidationContext::Field.validate("user").is_ok());
        assert!(ValidationContext::Field.validate("blog_post").is_ok());
        assert!(ValidationContext::Field.validate("item123").is_ok());
    }

    #[test]
    fn test_validate_field_name_invalid() {
        assert!(ValidationContext::Field.validate("").is_err());
        assert!(ValidationContext::Field.validate("User").is_err());
        assert!(ValidationContext::Field.validate("_user").is_err());
        assert!(ValidationContext::Field.validate("user_").is_err());
        assert!(ValidationContext::Field.validate("self").is_err());
        assert!(ValidationContext::Field.validate("user-name").is_err());
    }

    #[test]
    fn test_normalized_type_from_str() {
        use std::str::FromStr;

        // Primitives and common aliases
        assert_eq!(
            NormalizedType::from_str("string").unwrap(),
            NormalizedType::String
        );
        assert_eq!(
            NormalizedType::from_str("str").unwrap(),
            NormalizedType::String
        );
        assert_eq!(
            NormalizedType::from_str("text").unwrap(),
            NormalizedType::Text
        );
        assert_eq!(
            NormalizedType::from_str("i32").unwrap(),
            NormalizedType::I32
        );
        assert_eq!(
            NormalizedType::from_str("integer").unwrap(),
            NormalizedType::I32
        );
        assert_eq!(
            NormalizedType::from_str("int").unwrap(),
            NormalizedType::I32
        );
        assert_eq!(
            NormalizedType::from_str("i64").unwrap(),
            NormalizedType::I64
        );
        assert_eq!(
            NormalizedType::from_str("bigint").unwrap(),
            NormalizedType::I64
        );
        assert_eq!(
            NormalizedType::from_str("f32").unwrap(),
            NormalizedType::F32
        );
        assert_eq!(
            NormalizedType::from_str("float").unwrap(),
            NormalizedType::F32
        );
        assert_eq!(
            NormalizedType::from_str("f64").unwrap(),
            NormalizedType::F64
        );
        assert_eq!(
            NormalizedType::from_str("double").unwrap(),
            NormalizedType::F64
        );
        assert_eq!(
            NormalizedType::from_str("bool").unwrap(),
            NormalizedType::Bool
        );
        assert_eq!(
            NormalizedType::from_str("boolean").unwrap(),
            NormalizedType::Bool
        );
        assert_eq!(
            NormalizedType::from_str("uuid").unwrap(),
            NormalizedType::Uuid
        );

        // Date/Time
        assert_eq!(
            NormalizedType::from_str("datetimeutc").unwrap(),
            NormalizedType::DateTimeUtc
        );
        assert_eq!(
            NormalizedType::from_str("timestamptz").unwrap(),
            NormalizedType::DateTimeUtc
        );
        assert_eq!(
            NormalizedType::from_str("naivedatetime").unwrap(),
            NormalizedType::DateTime
        );
        assert_eq!(
            NormalizedType::from_str("timestamp").unwrap(),
            NormalizedType::DateTime
        );
        assert_eq!(
            NormalizedType::from_str("date").unwrap(),
            NormalizedType::Date
        );
        assert_eq!(
            NormalizedType::from_str("time").unwrap(),
            NormalizedType::Time
        );

        // Special types
        assert_eq!(
            NormalizedType::from_str("decimal").unwrap(),
            NormalizedType::Decimal
        );
        assert_eq!(
            NormalizedType::from_str("numeric").unwrap(),
            NormalizedType::Decimal
        );
        assert_eq!(
            NormalizedType::from_str("money").unwrap(),
            NormalizedType::Decimal
        );
        assert_eq!(
            NormalizedType::from_str("json").unwrap(),
            NormalizedType::Json
        );
        assert_eq!(
            NormalizedType::from_str("jsonb").unwrap(),
            NormalizedType::Json
        );
        assert_eq!(
            NormalizedType::from_str("bytes").unwrap(),
            NormalizedType::Bytes
        );
        assert_eq!(
            NormalizedType::from_str("blob").unwrap(),
            NormalizedType::Bytes
        );
        assert_eq!(
            NormalizedType::from_str("binary").unwrap(),
            NormalizedType::Bytes
        );
        assert_eq!(
            NormalizedType::from_str("bytea").unwrap(),
            NormalizedType::Bytes
        );
    }

    #[test]
    fn test_normalized_type_schema_name() {
        let filds = vec![
            (NormalizedType::String, "String"),
            (NormalizedType::I32, "i32"),
            (NormalizedType::Bool, "bool"),
            (NormalizedType::Date, "Date"),
            (NormalizedType::Time, "Time"),
            (NormalizedType::DateTime, "NaiveDateTime"),
            (NormalizedType::DateTimeUtc, "DateTime"),
            (NormalizedType::Decimal, "Decimal"),
            (NormalizedType::Json, "Json"),
            (NormalizedType::Bytes, "Vec<u8>"),
        ];

        for (normalized_type, expected) in filds {
            assert_eq!(schema_type_name(&normalized_type, false), expected);
        }
    }

    #[test]
    fn test_column_method_new() {
        // String field
        let field = "title:string".parse::<FieldInfo>().unwrap();
        assert_eq!(
            field.generate_column("Post"),
            ".col(ColumnDef::new(Post::Title).string().not_null())"
        );

        // Primary key i32
        let field = "id:i32".parse::<FieldInfo>().unwrap();
        assert_eq!(
            field.generate_column("Posts"),
            ".col(ColumnDef::new(Posts::Id).integer().not_null().primary_key().auto_increment())"
        );

        // Primary key i64
        let field = "id:i64".parse::<FieldInfo>().unwrap();
        assert_eq!(
            field.generate_column("Users"),
            ".col(ColumnDef::new(Users::Id).big_integer().not_null().primary_key().auto_increment())"
        );

        // Uuid field
        let field = "token:uuid".parse::<FieldInfo>().unwrap();
        assert_eq!(
            field.generate_column("Auth"),
            ".col(ColumnDef::new(Auth::Token).uuid().not_null())"
        );

        // Nullable field (manually set as from_str defaults to false)
        let mut field = "bio:text".parse::<FieldInfo>().unwrap();
        field.nullable = true;
        assert_eq!(
            field.generate_column("Profile"),
            ".col(ColumnDef::new(Profile::Bio).text().null())"
        );
    }

    #[test]
    fn test_sea_orm_import_name() {
        assert_eq!(
            NormalizedType::DateTimeUtc.sea_orm_import_name(),
            Some("DateTimeUtc")
        );
        assert_eq!(
            NormalizedType::DateTime.sea_orm_import_name(),
            Some("DateTime")
        );
        assert_eq!(NormalizedType::Date.sea_orm_import_name(), Some("Date"));
        assert_eq!(NormalizedType::Json.sea_orm_import_name(), Some("Json"));
        assert_eq!(NormalizedType::Time.sea_orm_import_name(), Some("Time"));
        assert_eq!(NormalizedType::String.sea_orm_import_name(), None);
    }
}
