//! Health check handlers for `/__rapina/health`, `/__rapina/health/live`,
//! and `/__rapina/health/ready`.
//!
//! - [`liveness_check`] — always returns `200 OK`. Used by Kubernetes liveness
//!   probes to detect deadlocked or crashed processes. Failure triggers a pod restart.
//! - [`readiness_check`] — runs DB and custom checks. Used by Kubernetes readiness
//!   probes to gate traffic. Failure removes the pod from the load balancer without restarting it.
//! - [`health_check`] — alias for `readiness_check`, registered at `/__rapina/health`
//!   for backwards compatibility and simple deployments.

use std::sync::Arc;

use http::{Request, Response, StatusCode, header::CONTENT_TYPE};
use hyper::body::Incoming;

use crate::{
    extract::PathParams,
    health::config::HealthRegistry,
    response::{APPLICATION_JSON, BoxBody, full},
    state::AppState,
};

/// Handler for `GET /__rapina/health/live`.
///
/// Always returns `200 OK` with `{"status": "ok"}` as long as the process is running.
/// No external dependencies are checked — this probe only answers "is the process alive?".
///
/// A failure here causes Kubernetes to restart the pod, so it should never fail
/// due to a database outage or external service being down.
pub async fn liveness_check(
    _req: Request<Incoming>,
    _params: PathParams,
    _state: Arc<AppState>,
) -> Response<BoxBody> {
    let body = serde_json::json!({ "status": "ok" });
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, APPLICATION_JSON)
        .body(full(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap()
}

/// Handler for `GET /__rapina/health/ready` and `GET /__rapina/health`.
///
/// Runs all configured health checks (database connectivity and custom checks)
/// and returns a unified JSON response. Returns `200 OK` when everything is
/// healthy, `503 Service Unavailable` when any check fails.
///
/// A failure here causes Kubernetes to stop routing traffic to the pod without
/// restarting it — the right behaviour for transient dependency outages.
pub async fn readiness_check(
    _req: Request<Incoming>,
    _params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    let mut checks = serde_json::Map::new();
    let mut all_ok = true;

    // Database connectivity check — only compiled when the `database` feature is enabled.
    // Runs a lightweight `SELECT 1` against the active connection to verify reachability.
    #[cfg(feature = "database")]
    {
        use sea_orm::{ConnectionTrait, Statement};

        if let Some(conn) = state.get::<sea_orm::DatabaseConnection>() {
            let backend = conn.get_database_backend();
            let db_ok = conn
                .execute(Statement::from_string(backend, "SELECT 1"))
                .await
                .is_ok();
            checks.insert(
                "db".to_string(),
                if db_ok { "ok".into() } else { "error".into() },
            );
            if !db_ok {
                all_ok = false;
            }
        }
    }

    // Custom checks registered via `.add_health_check(name, fn)` on the builder.
    // Each check is called sequentially; all results are collected before responding.
    if let Some(registry) = state.get::<HealthRegistry>() {
        for (name, check_fn) in &registry.checks {
            let ok = check_fn().await;
            checks.insert(
                name.to_string(),
                if ok { "ok".into() } else { "error".into() },
            );
            if !ok {
                all_ok = false;
            }
        }
    }

    // Build the response body. The `"checks"` key is omitted when no checks are configured
    // so that the basic case stays as simple as `{"status": "ok"}`.
    let mut body = serde_json::json!({ "status": if all_ok { "ok" } else { "error" } });
    if !checks.is_empty() {
        body["checks"] = serde_json::Value::Object(checks);
    }
    let status = if all_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, APPLICATION_JSON)
        .body(full(serde_json::to_vec(&body).unwrap_or_default()))
        .unwrap()
}

/// Alias for [`readiness_check`], registered at `GET /__rapina/health`.
pub async fn health_check(
    req: Request<Incoming>,
    params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    readiness_check(req, params, state).await
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, StatusCode};
    use serde_json::Value;

    use crate::{app::Rapina, testing::TestClient};

    #[tokio::test]
    async fn test_liveness_check_always_returns_200() {
        let app = Rapina::new().with_health_check(true);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health/live").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>()["status"], "ok");
    }

    #[tokio::test]
    async fn test_readiness_check_returns_200_when_no_checks() {
        let app = Rapina::new().with_health_check(true);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health/ready").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>()["status"], "ok");
    }

    #[tokio::test]
    async fn test_readiness_check_returns_503_when_custom_check_fails() {
        let app = Rapina::new()
            .with_health_check(true)
            .add_health_check("redis", || async { false });
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health/ready").send().await;

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn test_health_check_returns_200_with_json_content_type() {
        let app = Rapina::new().with_health_check(true);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(http::header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
    }

    #[tokio::test]
    async fn test_health_check_returns_status_ok() {
        let app = Rapina::new().with_health_check(true);
        let client = TestClient::new(app).await;
        let json = client.get("/__rapina/health").send().await.json::<Value>();

        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn test_health_check_returns_404_when_disabled() {
        let app = Rapina::new().with_health_check(false);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health").send().await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_health_check_with_passing_custom_check_returns_200() {
        let app = Rapina::new()
            .with_health_check(true)
            .add_health_check("redis", || async { true });
        let client = TestClient::new(app).await;
        let json = client.get("/__rapina/health").send().await.json::<Value>();

        assert_eq!(json["status"], "ok");
        assert_eq!(json["checks"]["redis"], "ok");
    }

    #[tokio::test]
    async fn test_health_check_with_failing_custom_check_returns_503() {
        let app = Rapina::new()
            .with_health_check(true)
            .add_health_check("redis", || async { false });
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health").send().await;
        let status = response.status();
        let json = response.json::<Value>();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["status"], "error");
        assert_eq!(json["checks"]["redis"], "error");
    }

    #[tokio::test]
    async fn test_health_check_with_mixed_custom_checks_returns_503() {
        let app = Rapina::new()
            .with_health_check(true)
            .add_health_check("redis", || async { true })
            .add_health_check("stripe", || async { false });
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/health").send().await;
        let status = response.status();
        let json = response.json::<Value>();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(json["status"], "error");
        assert_eq!(json["checks"]["redis"], "ok");
        assert_eq!(json["checks"]["stripe"], "error");
    }

    #[cfg(feature = "sqlite")]
    #[tokio::test]
    async fn test_health_check_with_database_returns_db_ok() {
        use crate::database::DatabaseConfig;

        let app = Rapina::new()
            .with_health_check(true)
            .with_database(DatabaseConfig::new("sqlite::memory:"))
            .await
            .unwrap();
        let client = TestClient::new(app).await;
        let json = client.get("/__rapina/health").send().await.json::<Value>();

        assert_eq!(json["status"], "ok");
        assert_eq!(json["checks"]["db"], "ok");
    }
}
