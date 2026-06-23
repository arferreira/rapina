//! Shared parser for `schema!` blocks in `src/entity.rs`, used by `seed` and `import database --diff`.

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedEntity {
    pub name: String,
    pub table_name: Option<String>,
    pub primary_key: Option<Vec<String>>,
    pub has_created_at: bool,
    pub has_updated_at: bool,
    pub fields: Vec<ParsedField>,
}

#[derive(Debug, PartialEq)]
pub(crate) struct ParsedField {
    pub name: String,
    pub column: Option<String>,
    pub type_str: String,
}

pub(crate) fn parse_schema_content(content: &str) -> Result<Vec<ParsedEntity>, String> {
    let mut entities = Vec::new();
    let mut lines = content.lines();

    while let Some(line) = lines.next() {
        if line.trim().starts_with("schema!") {
            // One block can hold several entities (the macro parses them with
            // `while !input.is_empty()`), so keep going until the block closes.
            while let Some(entity) = parse_next_entity(&mut lines)? {
                entities.push(entity);
            }
        }
    }

    if entities.is_empty() {
        return Err("No schema! blocks found in src/entity.rs".to_string());
    }

    Ok(entities)
}

fn parse_next_entity(lines: &mut std::str::Lines) -> Result<Option<ParsedEntity>, String> {
    let mut pending: Vec<&str> = Vec::new();

    // buffer entity attrs, then the next real line is the entity name.
    let name = loop {
        let line = lines.next().ok_or("Unexpected end of schema! block")?;
        let t = line.trim();
        if t.is_empty() || t == "{" {
            continue;
        }
        if t == "}" {
            // closing brace of the block, not an entity: nothing left to parse.
            return Ok(None);
        }
        if t.starts_with("#[") {
            pending.push(t);
            continue;
        }
        break t.strip_suffix('{').unwrap_or(t).trim().to_string();
    };

    let mut entity = ParsedEntity {
        name,
        table_name: None,
        primary_key: None,
        has_created_at: true,
        has_updated_at: true,
        fields: Vec::new(),
    };

    for attr in pending.iter().copied() {
        apply_entity_attr(attr, &mut entity);
    }
    pending.clear();

    // collect fields, buffering field attrs until the field line consumes them.
    // Entity bodies have no nested braces, so the entity closes on the first `}`,
    // leaving the block's `}` on its own line for the next call to consume.
    for line in lines.by_ref() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if t == "}" {
            break;
        }
        if t.starts_with("#[") {
            pending.push(t);
            continue;
        }
        if let Some((field_name, type_str)) = t.trim_end_matches(',').split_once(':') {
            let mut column = None;
            for attr in pending.iter().copied() {
                if attr_name(attr) == "column" {
                    column = quoted_value(attr);
                }
            }
            entity.fields.push(ParsedField {
                name: field_name.trim().to_string(),
                column,
                type_str: type_str.trim().to_string(),
            });
        }
        pending.clear();
    }

    Ok(Some(entity))
}

fn apply_entity_attr(line: &str, entity: &mut ParsedEntity) {
    match attr_name(line) {
        "table_name" => entity.table_name = quoted_value(line),
        "primary_key" => {
            entity.primary_key = paren_inner(line)
                .map(|inner| inner.split(',').map(|p| p.trim().to_string()).collect());
        }
        "timestamps" => match paren_inner(line).map(str::trim) {
            Some("none") => {
                entity.has_created_at = false;
                entity.has_updated_at = false;
            }
            Some("created_at") => entity.has_updated_at = false,
            Some("updated_at") => entity.has_created_at = false,
            _ => {}
        },
        _ => {}
    }
}

fn attr_name(s: &str) -> &str {
    s.trim_start_matches("#[")
        .split(['(', '=', ']'])
        .next()
        .unwrap_or("")
        .trim()
}

fn quoted_value(s: &str) -> Option<String> {
    let start = s.find('"')?;
    let end = s.rfind('"')?;
    (end > start).then(|| s[start + 1..end].to_string())
}

fn paren_inner(s: &str) -> Option<&str> {
    let start = s.find('(')?;
    let end = s.rfind(')')?;
    (end > start).then(|| &s[start + 1..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_entity_attrs() {
        let input = r#"
schema! {
    #[table_name = "people"]
    #[timestamps(created_at)]
    #[primary_key(id)]
    Person {
        name: String,
    }
}
"#;
        let entities = parse_schema_content(input).unwrap();
        assert_eq!(entities.len(), 1);
        let e = &entities[0];
        assert_eq!(e.name, "Person");
        assert_eq!(e.table_name.as_deref(), Some("people"));
        assert_eq!(e.primary_key, Some(vec!["id".to_string()]));
        assert!(e.has_created_at);
        assert!(!e.has_updated_at);
    }

    #[test]
    fn timestamps_default_to_both_on() {
        let input = "schema! {\n    User {\n        name: String,\n    }\n}";
        let e = &parse_schema_content(input).unwrap()[0];
        assert!(e.has_created_at);
        assert!(e.has_updated_at);
    }

    #[test]
    fn timestamps_none_turns_both_off() {
        let input =
            "schema! {\n    #[timestamps(none)]\n    User {\n        name: String,\n    }\n}";
        let e = &parse_schema_content(input).unwrap()[0];
        assert!(!e.has_created_at);
        assert!(!e.has_updated_at);
    }

    #[test]
    fn field_column_attr_attaches_and_buffer_clears() {
        let input = r#"
schema! {
    User {
        #[column = "full_name"]
        name: String,
        email: String,
    }
}
"#;
        let e = &parse_schema_content(input).unwrap()[0];
        assert_eq!(e.fields[0].column.as_deref(), Some("full_name"));
        assert_eq!(e.fields[1].column, None);
    }

    #[test]
    fn keeps_relationship_fields_raw() {
        let input = r#"
schema! {
    User {
        name: String,
        author: User,
        posts: Vec<Post>,
    }
}
"#;
        let e = &parse_schema_content(input).unwrap()[0];
        let types: Vec<&str> = e.fields.iter().map(|f| f.type_str.as_str()).collect();
        assert_eq!(types, vec!["String", "User", "Vec<Post>"]);
    }

    #[test]
    fn parses_multiple_entities() {
        let input = r#"
schema! {
    User {
        name: String,
    }
}
schema! {
    Post {
        title: String,
    }
}
"#;
        let entities = parse_schema_content(input).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].name, "User");
        assert_eq!(entities[1].name, "Post");
    }

    #[test]
    fn parses_multiple_entities_in_one_block() {
        let input = r#"
schema! {
    User {
        name: String,
    }
    Post {
        title: String,
    }
}
"#;
        let entities = parse_schema_content(input).unwrap();
        assert_eq!(entities.len(), 2);
        assert_eq!(entities[0].name, "User");
        assert_eq!(entities[1].name, "Post");
        assert_eq!(entities[1].fields[0].name, "title");
    }

    #[test]
    fn errors_when_no_schema_blocks() {
        let err = parse_schema_content("fn main() {}").unwrap_err();
        assert!(err.contains("No schema! blocks"));
    }
}
