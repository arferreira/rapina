//! Integration tests for route auto-discovery.
//!
//! IMPORTANT: `inventory` collects from the entire test binary.
//! All handlers across test files share the same collection.
//! Use unique `/disc-*` path prefixes to avoid collisions.

use http::StatusCode;
use rapina::prelude::*;
use rapina::testing::TestClient;

// ── Discovered handlers ─────────────────────────────────────────────────────

#[get("/disc-hello")]
async fn disc_hello() -> &'static str {
    "hello from discovery"
}

#[post("/disc-echo")]
async fn disc_echo() -> &'static str {
    "echoed"
}

#[put("/disc-update")]
async fn disc_update() -> &'static str {
    "updated"
}

#[delete("/disc-remove")]
async fn disc_remove() -> StatusCode {
    StatusCode::NO_CONTENT
}

// ── Public handlers (both orderings) ────────────────────────────────────────

// #[public] above #[get]
#[public]
#[get("/disc-pub-above")]
async fn disc_pub_above() -> &'static str {
    "public above"
}

// #[public] below #[get]
#[get("/disc-pub-below")]
#[public]
async fn disc_pub_below() -> &'static str {
    "public below"
}

// Non-public handler (should be blocked by auth)
#[get("/disc-protected")]
async fn disc_protected() -> &'static str {
    "protected"
}

// ── Grouped handlers ─────────────────────────────────────────────────────────

#[get("/grp-users", group = "/api")]
async fn grp_list_users() -> &'static str {
    "grouped users"
}

#[post("/grp-users", group = "/api")]
async fn grp_create_user() -> &'static str {
    "grouped create"
}

#[get("/grp-users/:id", group = "/api")]
async fn grp_get_user(id: Path<u64>) -> String {
    format!("grouped user {}", *id)
}

#[public]
#[get("/grp-health", group = "/api")]
async fn grp_public_health() -> &'static str {
    "grouped public health"
}

#[get("/grp-secret", group = "/api")]
async fn grp_protected() -> &'static str {
    "grouped secret"
}

#[put("/grp-items/:id", group = "/api")]
async fn grp_update_item(id: Path<u64>) -> String {
    format!("grouped update {}", *id)
}

#[delete("/grp-items/:id", group = "/api")]
async fn grp_delete_item() -> StatusCode {
    StatusCode::NO_CONTENT
}

// #[public] BELOW the route macro + group
#[get("/grp-open", group = "/api")]
#[public]
async fn grp_public_below() -> &'static str {
    "grouped public below"
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_discovered_get_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.get("/disc-hello").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "hello from discovery");
}

#[tokio::test]
async fn test_discovered_post_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.post("/disc-echo").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "echoed");
}

#[tokio::test]
async fn test_discovered_put_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.put("/disc-update").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "updated");
}

#[tokio::test]
async fn test_discovered_delete_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.delete("/disc-remove").send().await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_discover_and_router_are_additive() {
    let manual_router = Router::new().route(http::Method::GET, "/disc-manual", |_, _, _| async {
        "manual route"
    });

    let app = Rapina::new()
        .with_introspection(false)
        .router(manual_router)
        .discover();

    let client = TestClient::new(app).await;

    // Discovered route works
    let resp = client.get("/disc-hello").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "hello from discovery");

    // Manual route also works
    let resp = client.get("/disc-manual").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "manual route");
}

#[tokio::test]
async fn test_public_above_route_macro_bypasses_auth() {
    let auth_config = AuthConfig::new("test-secret-disc", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    // #[public] above #[get] — should be accessible without token
    let resp = client.get("/disc-pub-above").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "public above");
}

#[tokio::test]
async fn test_public_below_route_macro_bypasses_auth() {
    let auth_config = AuthConfig::new("test-secret-disc", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    // #[public] below #[get] — should be accessible without token
    let resp = client.get("/disc-pub-below").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "public below");
}

#[tokio::test]
async fn test_non_public_discovered_route_blocked_by_auth() {
    let auth_config = AuthConfig::new("test-secret-disc", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    // Non-public route should be blocked (401)
    let resp = client.get("/disc-protected").send().await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_discovered_routes_appear_in_introspection() {
    let app = Rapina::new().with_introspection(true).discover();

    let client = TestClient::new(app).await;
    let resp = client.get("/__rapina/routes").send().await;

    assert_eq!(resp.status(), StatusCode::OK);

    let routes: Vec<serde_json::Value> = resp.json();
    let paths: Vec<&str> = routes
        .iter()
        .filter_map(|r| r.get("path").and_then(|p| p.as_str()))
        .collect();

    assert!(paths.contains(&"/disc-hello"));
    assert!(paths.contains(&"/disc-echo"));
    assert!(paths.contains(&"/disc-pub-above"));
    assert!(paths.contains(&"/disc-pub-below"));
}

// ── Group tests ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn test_grouped_route_accessible_at_prefixed_path() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.get("/api/grp-users").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped users");
}

#[tokio::test]
async fn test_grouped_post_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.post("/api/grp-users").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped create");
}

#[tokio::test]
async fn test_grouped_route_with_path_param() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.get("/api/grp-users/42").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped user 42");
}

#[tokio::test]
async fn test_grouped_public_route_bypasses_auth() {
    let auth_config = AuthConfig::new("test-secret-grp", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    let resp = client.get("/api/grp-health").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped public health");
}

#[tokio::test]
async fn test_grouped_non_public_route_blocked_by_auth() {
    let auth_config = AuthConfig::new("test-secret-grp", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    let resp = client.get("/api/grp-secret").send().await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_grouped_routes_appear_in_introspection() {
    let app = Rapina::new().with_introspection(true).discover();

    let client = TestClient::new(app).await;
    let resp = client.get("/__rapina/routes").send().await;

    assert_eq!(resp.status(), StatusCode::OK);

    let routes: Vec<serde_json::Value> = resp.json();
    let paths: Vec<&str> = routes
        .iter()
        .filter_map(|r| r.get("path").and_then(|p| p.as_str()))
        .collect();

    assert!(paths.contains(&"/api/grp-users"));
    assert!(paths.contains(&"/api/grp-health"));
}

#[tokio::test]
async fn test_grouped_route_not_accessible_at_unprefixed_path() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    // The handler is registered at /api/grp-users, NOT at /grp-users.
    let resp = client.get("/grp-users").send().await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_grouped_put_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.put("/api/grp-items/7").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped update 7");
}

#[tokio::test]
async fn test_grouped_delete_route() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    let resp = client.delete("/api/grp-items/7").send().await;
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn test_grouped_public_below_route_macro_bypasses_auth() {
    let auth_config = AuthConfig::new("test-secret-grp", 3600);

    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(auth_config)
        .discover();

    let client = TestClient::new(app).await;

    // #[public] below #[get] + group — should still be accessible without token
    let resp = client.get("/api/grp-open").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "grouped public below");
}

#[tokio::test]
async fn test_grouped_route_wrong_method_returns_404() {
    let app = Rapina::new().with_introspection(false).discover();
    let client = TestClient::new(app).await;

    // grp_list_users is GET only, POST is a different handler
    let resp = client.delete("/api/grp-users").send().await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_grouped_introspection_shows_method_correctly() {
    let app = Rapina::new().with_introspection(true).discover();

    let client = TestClient::new(app).await;
    let resp = client.get("/__rapina/routes").send().await;
    assert_eq!(resp.status(), StatusCode::OK);

    let routes: Vec<serde_json::Value> = resp.json();

    // Find the grouped POST route and verify method + path are both correct
    let grouped_post = routes.iter().find(|r| {
        r.get("path").and_then(|p| p.as_str()) == Some("/api/grp-users")
            && r.get("method").and_then(|m| m.as_str()) == Some("POST")
    });
    assert!(
        grouped_post.is_some(),
        "POST /api/grp-users should appear in introspection"
    );

    // The unprefixed path should NOT appear
    let unprefixed = routes
        .iter()
        .find(|r| r.get("path").and_then(|p| p.as_str()) == Some("/grp-users"));
    assert!(
        unprefixed.is_none(),
        "/grp-users should not appear in introspection"
    );
}
