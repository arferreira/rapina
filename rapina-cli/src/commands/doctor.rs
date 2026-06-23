//! Health checks for your Rapina API.

use crate::commands::agents::{DriftStatus, check_drift, fix_agents, simple_diff};
use crate::common::urls;
use colored::Colorize;
use serde_json::Value;

struct DiagnosticResult {
    warnings: Vec<String>,
    errors: Vec<String>,
    passed: Vec<String>,
}

pub struct DoctorConfig {
    pub host: String,
    pub port: u16,
    pub fix_agents: bool,
    pub force: bool,
}

/// Run health checks on the API.
pub fn execute(config: DoctorConfig) -> Result<(), String> {
    println!();

    // ── Local checks (no server required) ────────────────────────────────────
    check_agents_drift(&config)?;

    // ── Server checks ─────────────────────────────────────────────────────────
    println!(
        "  {} Running API health checks on http://{}:{}...",
        "→".cyan(),
        config.host,
        config.port
    );
    println!();

    let routes = fetch_json(&urls::build_routes_url(&config.host, config.port))?;
    let openapi = fetch_json(&urls::build_openapi_url(&config.host, config.port));

    let mut result = DiagnosticResult {
        warnings: Vec::new(),
        errors: Vec::new(),
        passed: Vec::new(),
    };

    check_response_schemas(&routes, &mut result);
    check_error_documentation(&routes, &mut result);
    check_openapi_metadata(&openapi, &mut result);
    check_duplicate_routes(&routes, &mut result);
    check_llms_txt(&config.host, config.port, &mut result);

    print_results(&result);

    if !result.errors.is_empty() {
        Err(format!("Found {} error(s)", result.errors.len()))
    } else {
        Ok(())
    }
}

fn check_agents_drift(config: &DoctorConfig) -> Result<(), String> {
    match check_drift(std::path::Path::new(".")) {
        DriftStatus::UpToDate => {
            println!("  {} AGENTS.md is up to date", "✓".green());
        }
        DriftStatus::Missing => {
            if config.fix_agents {
                fix_agents(std::path::Path::new("."), config.force)?;
                println!("  {} Created AGENTS.md", "✓".green());
            } else {
                println!(
                    "  {} AGENTS.md not found — run {} to generate it",
                    "⚠".yellow(),
                    "rapina doctor --fix-agents".cyan()
                );
            }
        }
        DriftStatus::NoBlock => {
            if config.fix_agents {
                fix_agents(std::path::Path::new("."), config.force)?;
                println!(
                    "  {} Injected rapina-agent-rules block into AGENTS.md",
                    "✓".green()
                );
            } else {
                println!(
                    "  {} AGENTS.md has no rapina-agent-rules block — run {} to add one",
                    "⚠".yellow(),
                    "rapina doctor --fix-agents".cyan()
                );
            }
        }
        DriftStatus::Stale { stored_version } => {
            if config.fix_agents {
                fix_agents(std::path::Path::new("."), config.force)?;
                println!("  {} AGENTS.md refreshed", "✓".green());
            } else {
                println!(
                    "  {} AGENTS.md is stale (was v{stored_version}, now v{}) — run {} to refresh",
                    "⚠".yellow(),
                    env!("CARGO_PKG_VERSION"),
                    "rapina doctor --fix-agents".cyan()
                );
            }
        }
        DriftStatus::UserEdited {
            on_disk_body,
            current_body,
        } => {
            if config.fix_agents && config.force {
                fix_agents(std::path::Path::new("."), true)?;
                println!("  {} AGENTS.md overwritten (--force)", "✓".green());
            } else {
                println!(
                    "  {} AGENTS.md has been edited inside the {}...{} markers",
                    "✗".red(),
                    "<!-- BEGIN:rapina-agent-rules".cyan(),
                    "-->".cyan()
                );
                println!();
                println!("  Move your custom rules outside the markers, then run:");
                println!("    {}", "rapina doctor --fix-agents".cyan());
                println!();
                println!(
                    "  To overwrite your edits: {}",
                    "rapina doctor --fix-agents --force".cyan()
                );
                println!();
                println!(
                    "  Diff (on-disk vs current bundled fragments; grouped: removals first, then additions):"
                );
                for line in simple_diff(&on_disk_body, &current_body).lines() {
                    if line.starts_with('-') {
                        println!("    {}", line.red());
                    } else if line.starts_with('+') {
                        println!("    {}", line.green());
                    } else {
                        println!("    {}", line);
                    }
                }
                println!();
                return Err("AGENTS.md has user edits inside the markers".to_string());
            }
        }
        DriftStatus::NotInProject => {
            println!(
                "  {} Not in a Rapina project — skipping AGENTS.md check",
                "⚠".yellow()
            );
        }
    }
    Ok(())
}

/// Check that routes have response schemas.
fn check_response_schemas(routes: &Value, result: &mut DiagnosticResult) {
    let routes_array = match routes.as_array() {
        Some(arr) => arr,
        None => return,
    };

    let mut missing = Vec::new();

    for route in routes_array {
        let path = route.get("path").and_then(|p| p.as_str()).unwrap_or("?");
        let method = route.get("method").and_then(|m| m.as_str()).unwrap_or("?");
        let has_schema = route.get("response_schema").is_some();

        if !has_schema && !path.starts_with("/__rapina") {
            missing.push(format!("{} {}", method, path));
        }
    }

    if missing.is_empty() {
        result
            .passed
            .push("All routes have response schemas".to_string());
    } else {
        for route in missing {
            result
                .warnings
                .push(format!("Missing response schema: {}", route));
        }
    }
}

/// Check that routes have error documentation.
fn check_error_documentation(routes: &Value, result: &mut DiagnosticResult) {
    let routes_array = match routes.as_array() {
        Some(arr) => arr,
        None => return,
    };

    let mut missing = Vec::new();

    for route in routes_array {
        let path = route.get("path").and_then(|p| p.as_str()).unwrap_or("?");
        let method = route.get("method").and_then(|m| m.as_str()).unwrap_or("?");
        let error_responses = route
            .get("error_responses")
            .and_then(|e| e.as_array())
            .map(|a| a.len())
            .unwrap_or(0);

        if error_responses == 0 && !path.starts_with("/__rapina") {
            missing.push(format!("{} {}", method, path));
        }
    }

    if missing.is_empty() {
        result
            .passed
            .push("All routes have documented errors".to_string());
    } else {
        for route in missing {
            result
                .warnings
                .push(format!("No documented errors: {}", route));
        }
    }
}

/// Check for duplicate (method, path) pairs — only one handler is used, others are shadowed.
fn check_duplicate_routes(routes: &Value, result: &mut DiagnosticResult) {
    let routes_array = match routes.as_array() {
        Some(arr) => arr,
        None => return,
    };

    let mut by_key: std::collections::HashMap<(String, String), Vec<String>> =
        std::collections::HashMap::new();

    for route in routes_array {
        let path = route
            .get("path")
            .and_then(|p| p.as_str())
            .unwrap_or("?")
            .to_string();
        if path.starts_with("/__rapina") {
            continue;
        }
        let method = route
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("?")
            .to_string();
        let handler_name = route
            .get("handler_name")
            .and_then(|h| h.as_str())
            .unwrap_or("?")
            .to_string();

        by_key.entry((method, path)).or_default().push(handler_name);
    }

    for ((method, path), handlers) in &by_key {
        if handlers.len() <= 1 {
            continue;
        }
        let list = handlers.join(", ");
        result.warnings.push(format!(
            "Duplicate route {} {}: handlers [{}] — only the first match is used, others are shadowed",
            method, path, list
        ));
    }

    if !by_key.is_empty() && by_key.values().all(|v| v.len() <= 1) {
        result.passed.push("No duplicate handler paths".to_string());
    }
}

/// Check OpenAPI metadata.
fn check_openapi_metadata(openapi: &Result<Value, String>, result: &mut DiagnosticResult) {
    let openapi = match openapi {
        Ok(openapi) => openapi,
        Err(_) => {
            result
                .warnings
                .push("OpenAPI endpoint: not enabled (add .openapi() to enable)".to_string());
            return;
        }
    };

    let paths = match openapi.get("paths").and_then(|p| p.as_object()) {
        Some(p) => p,
        None => return,
    };

    let mut missing_summary = Vec::new();

    for (path, item) in paths {
        if path.starts_with("/__rapina") {
            continue;
        }

        for method in ["get", "post", "put", "delete"] {
            if let Some(operation) = item.get(method) {
                let has_summary = operation.get("summary").is_some();
                let has_description = operation.get("description").is_some();

                if !has_summary && !has_description {
                    missing_summary.push(format!("{} {}", method.to_uppercase(), path));
                }
            }
        }
    }

    if missing_summary.is_empty() {
        result
            .passed
            .push("All operations have descriptions".to_string());
    } else {
        for op in missing_summary {
            result
                .warnings
                .push(format!("Missing documentation: {}", op));
        }
    }
}

/// Check that /__rapina/llms.txt is reachable and not returning 404.
fn check_llms_txt(host: &str, port: u16, result: &mut DiagnosticResult) {
    let url = urls::build_llms_url(host, port);
    match crate::common::AGENT.get(&url).call() {
        Ok(_) => {
            result
                .passed
                .push("llms.txt is enabled and reachable".to_string());
        }
        Err(ureq::Error::StatusCode(404)) => {
            result.warnings.push(
                "llms.txt endpoint returns 404 — disabled in release builds by default. \
                Call enable_llms_txt() on your App to turn it on."
                    .to_string(),
            );
        }
        Err(ureq::Error::StatusCode(status)) => {
            result.warnings.push(format!(
                "llms.txt endpoint returned unexpected status {status} — expected 200"
            ));
        }
        Err(_) => {
            result.warnings.push(
                "llms.txt endpoint is unreachable — disabled in release builds by default. \
                Call enable_llms_txt() on your App to turn it on."
                    .to_string(),
            );
        }
    }
}

/// Print diagnostic results.
fn print_results(result: &DiagnosticResult) {
    // Print passed checks
    for msg in &result.passed {
        println!("  {} {}", "✓".green(), msg);
    }

    // Print warnings
    for msg in &result.warnings {
        println!("  {} {}", "⚠".yellow(), msg);
    }

    // Print errors
    for msg in &result.errors {
        println!("  {} {}", "✗".red(), msg);
    }

    println!();

    // Summary
    println!(
        "  {} {} passed, {} warnings, {} errors",
        "Summary:".bold(),
        result.passed.len().to_string().green(),
        result.warnings.len().to_string().yellow(),
        result.errors.len().to_string().red()
    );
    println!();

    if result.warnings.is_empty() && result.errors.is_empty() {
        println!("  Your API is healthy.");
    } else if result.errors.is_empty() {
        println!("  Consider addressing the warnings above.");
    }
    println!();
}

/// Fetch JSON from URL.
fn fetch_json(url: &str) -> Result<Value, String> {
    let mut response = crate::common::AGENT.get(url).call().map_err(|e| {
        format!(
            "Failed to fetch data. Is the server running? ({}) ({})",
            url, e
        )
    })?;
    let body = response
        .body_mut()
        .read_to_string()
        .map_err(|e| format!("Failed to read response body: {}", e))?;

    serde_json::from_str(&body).map_err(|e| format!("Invalid JSON response: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_duplicate_routes_warns_on_same_method_path() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users", "handler_name": "list_users"},
            {"method": "GET", "path": "/users", "handler_name": "other_list"},
            {"method": "POST", "path": "/users", "handler_name": "create_user"},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_duplicate_routes(&routes, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("Duplicate route GET /users"));
        assert!(result.warnings[0].contains("list_users"));
        assert!(result.warnings[0].contains("other_list"));
        assert!(result.warnings[0].contains("shadowed"));
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_duplicate_routes_ignores_internal_routes() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/__rapina/routes", "handler_name": "list_routes"},
            {"method": "GET", "path": "/__rapina/routes", "handler_name": "other"},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_duplicate_routes(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_duplicate_routes_passed_when_no_duplicates() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users", "handler_name": "list_users"},
            {"method": "POST", "path": "/users", "handler_name": "create_user"},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_duplicate_routes(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
        assert_eq!(result.passed[0], "No duplicate handler paths");
    }

    #[test]
    fn check_response_schemas_passes_when_all_have_schemas() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users", "response_schema": {"type": "array"}},
            {"method": "POST", "path": "/users", "response_schema": {"type": "object"}},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_response_schemas(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
        assert_eq!(result.passed[0], "All routes have response schemas");
    }

    #[test]
    fn check_response_schemas_warns_on_missing_schema() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users"},
            {"method": "POST", "path": "/users", "response_schema": {"type": "object"}},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_response_schemas(&routes, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("GET /users"));
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_response_schemas_skips_internal_routes() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/__rapina/routes"},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_response_schemas(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
    }

    #[test]
    fn check_error_documentation_passes_when_all_documented() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users", "error_responses": [{"status": 404}]},
            {"method": "POST", "path": "/users", "error_responses": [{"status": 422}]},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_error_documentation(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
        assert_eq!(result.passed[0], "All routes have documented errors");
    }

    #[test]
    fn check_error_documentation_warns_on_missing_errors() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/users"},
            {"method": "DELETE", "path": "/users/:id", "error_responses": []},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_error_documentation(&routes, &mut result);
        assert_eq!(result.warnings.len(), 2);
        assert!(result.warnings[0].contains("GET /users"));
        assert!(result.warnings[1].contains("DELETE /users/:id"));
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_error_documentation_skips_internal_routes() {
        let routes = serde_json::json!([
            {"method": "GET", "path": "/__rapina/openapi"},
        ]);
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_error_documentation(&routes, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
    }

    #[test]
    fn check_openapi_metadata_passes_when_all_documented() {
        let openapi: Result<Value, String> = Ok(serde_json::json!({
            "paths": {
                "/users": {
                    "get": {"summary": "List users"},
                    "post": {"description": "Create a user"}
                }
            }
        }));
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_openapi_metadata(&openapi, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
        assert_eq!(result.passed[0], "All operations have descriptions");
    }

    #[test]
    fn check_openapi_metadata_warns_on_missing_docs() {
        let openapi: Result<Value, String> = Ok(serde_json::json!({
            "paths": {
                "/users": {
                    "get": {},
                    "post": {"summary": "Create a user"}
                }
            }
        }));
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_openapi_metadata(&openapi, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("GET /users"));
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_openapi_metadata_warns_when_openapi_not_enabled() {
        let openapi: Result<Value, String> = Err("not found".to_string());
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_openapi_metadata(&openapi, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("not enabled"));
    }

    #[test]
    fn check_openapi_metadata_skips_internal_paths() {
        let openapi: Result<Value, String> = Ok(serde_json::json!({
            "paths": {
                "/__rapina/routes": {
                    "get": {}
                }
            }
        }));
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_openapi_metadata(&openapi, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
    }

    #[test]
    fn test_fetch_json_success() {
        let json = r#"{"status":"ok"}"#;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            use std::io::Read;
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                json.len(),
                json
            );
            use std::io::Write;
            stream.write_all(response.as_bytes()).unwrap();
        });
        let result = fetch_json(&format!("http://127.0.0.1:{}", port));
        handle.join().unwrap();
        assert_eq!(result.unwrap(), serde_json::json!({"status": "ok"}));
    }

    #[test]
    fn test_fetch_json_connection_refused() {
        let result = fetch_json("http://127.0.0.1:1");
        assert!(result.is_err());
    }

    fn serve_once(status: u16, body: &'static str) -> u16 {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = [0; 4096];
            use std::io::Read;
            let _ = stream.read(&mut buf);
            let status_text = if status == 200 { "OK" } else { "Not Found" };
            let response = format!(
                "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            use std::io::Write;
            stream.write_all(response.as_bytes()).unwrap();
        });
        port
    }

    #[test]
    fn check_llms_txt_passes_when_200() {
        let port = serve_once(200, "# LLMs\n");
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_llms_txt("127.0.0.1", port, &mut result);
        assert!(result.warnings.is_empty());
        assert_eq!(result.passed.len(), 1);
        assert!(result.passed[0].contains("llms.txt"));
    }

    #[test]
    fn check_llms_txt_warns_when_404() {
        let port = serve_once(404, "");
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_llms_txt("127.0.0.1", port, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("404"));
        assert!(result.warnings[0].contains("enable_llms_txt()"));
        assert!(result.passed.is_empty());
    }

    #[test]
    fn check_llms_txt_warns_when_unreachable() {
        let mut result = DiagnosticResult {
            warnings: Vec::new(),
            errors: Vec::new(),
            passed: Vec::new(),
        };
        check_llms_txt("127.0.0.1", 1, &mut result);
        assert_eq!(result.warnings.len(), 1);
        assert!(result.warnings[0].contains("unreachable"));
        assert!(result.warnings[0].contains("enable_llms_txt()"));
        assert!(result.passed.is_empty());
    }
}
