//! Integration tests for the Prometheus metrics feature.

#![cfg(feature = "metrics")]

use http::StatusCode;
use rapina::metrics::MetricsRegistry;
use rapina::prelude::*;
use rapina::testing::TestClient;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

// ── helpers ──────────────────────────────────────────────────────────────────

fn app_with_metrics() -> rapina::app::Rapina {
    Rapina::new()
        .with_introspection(false)
        .with_metrics(true)
        .router(
            Router::new()
                .route(http::Method::GET, "/health", |_, _, _| async { "ok" })
                .route(http::Method::GET, "/users/:id", |_, _, _| async {
                    StatusCode::OK
                })
                .route(http::Method::POST, "/users", |_, _, _| async {
                    StatusCode::CREATED
                }),
        )
}

// ── /metrics endpoint ─────────────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_endpoint_returns_200() {
    let client = TestClient::new(app_with_metrics()).await;
    let response = client.get("/metrics").send().await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_metrics_endpoint_content_type() {
    let client = TestClient::new(app_with_metrics()).await;
    let response = client.get("/metrics").send().await;

    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header missing")
        .to_str()
        .unwrap();

    assert!(content_type.contains("text/plain"));
    assert!(content_type.contains("version=0.0.4"));
}

#[tokio::test]
async fn test_metrics_endpoint_contains_all_metric_names() {
    let client = TestClient::new(app_with_metrics()).await;

    // Generate one real request so CounterVec/HistogramVec emit HELP+TYPE lines.
    client.get("/health").send().await;

    let body = client.get("/metrics").send().await.text();

    assert!(body.contains("http_requests_total"));
    assert!(body.contains("http_request_duration_seconds"));
    assert!(body.contains("http_requests_in_flight"));
}

#[tokio::test]
async fn test_metrics_endpoint_prometheus_format() {
    let client = TestClient::new(app_with_metrics()).await;
    let body = client.get("/metrics").send().await.text();

    assert!(body.contains("# HELP"));
    assert!(body.contains("# TYPE"));
}

// ── counter increments ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_counter_increments_on_request() {
    let client = TestClient::new(app_with_metrics()).await;

    client.get("/health").send().await;

    let body = client.get("/metrics").send().await.text();
    // After one GET /health 200, the counter label set must appear
    assert!(body.contains(r#"method="GET""#));
    assert!(body.contains(r#"path="/health""#));
    assert!(body.contains(r#"status="200""#));
}

#[tokio::test]
async fn test_metrics_counter_accumulates() {
    let client = TestClient::new(app_with_metrics()).await;

    client.get("/health").send().await;
    client.get("/health").send().await;
    client.get("/health").send().await;

    let body = client.get("/metrics").send().await.text();
    // Three requests → counter value 3 (plus the /metrics call itself, but different labels)
    assert!(body.contains(r#"path="/health""#));
    // The line for GET /health 200 should show 3
    assert!(body.contains("} 3"));
}

#[tokio::test]
async fn test_metrics_duration_histogram_populated() {
    let client = TestClient::new(app_with_metrics()).await;

    client.get("/health").send().await;

    let body = client.get("/metrics").send().await.text();
    // Histogram emits _bucket, _sum, _count suffixes
    assert!(body.contains("http_request_duration_seconds_bucket"));
    assert!(body.contains("http_request_duration_seconds_sum"));
    assert!(body.contains("http_request_duration_seconds_count"));
}

// ── route-pattern labelling ───────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_label_uses_matched_route_pattern() {
    let client = TestClient::new(app_with_metrics()).await;

    // Numeric, UUID, and slug params all hit the same registered route. The old
    // numeric-only heuristic collapsed the first and let the other two through
    // verbatim, one fresh time series each.
    client.get("/users/42").send().await;
    client
        .get("/users/e58ed763-928c-4155-bee9-fdbaaadc15f6")
        .send()
        .await;
    client.get("/users/whatever").send().await;

    let body = client.get("/metrics").send().await.text();

    // Every variant collapses to the route the developer registered.
    assert!(body.contains(r#"path="/users/:id""#));
    // No raw param value leaks into label cardinality.
    assert!(!body.contains(r#"path="/users/42""#));
    assert!(!body.contains("e58ed763-928c-4155-bee9-fdbaaadc15f6"));
    assert!(!body.contains(r#"path="/users/whatever""#));
}

#[tokio::test]
async fn test_metrics_unmatched_path_uses_sentinel() {
    let client = TestClient::new(app_with_metrics()).await;

    // A request that matches no route. Without a sentinel, a scanner spraying
    // random URLs would mint a new series per request.
    client
        .get("/nope/2f1c8e90-1111-2222-3333-444455556666")
        .send()
        .await;

    let body = client.get("/metrics").send().await.text();

    assert!(body.contains(r#"path="<unmatched>""#));
    assert!(!body.contains("2f1c8e90-1111-2222-3333-444455556666"));
}

// ── disabled by default ───────────────────────────────────────────────────────

#[tokio::test]
async fn test_metrics_disabled_by_default() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/metrics").send().await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

// ── MetricsRegistry unit-level via state ─────────────────────────────────────

#[test]
fn test_metrics_registry_new_does_not_panic() {
    let _r = MetricsRegistry::new();
}

#[test]
fn test_metrics_registry_encode_returns_text() {
    let r = MetricsRegistry::new();
    let out = r.encode();
    assert!(!out.is_empty());
    assert!(out.contains("# TYPE"));
}

// ── custom metrics via add_metric ────────────────────────────────────────────

#[tokio::test]
async fn test_custom_metric_appears_in_metrics_endpoint() {
    use rapina::prometheus::IntCounter;

    let counter = IntCounter::new("my_orders_total", "Total orders placed").unwrap();
    counter.inc();
    counter.inc();

    let app = Rapina::new()
        .with_introspection(false)
        .enable_metrics()
        .add_metric(counter);

    let client = TestClient::new(app).await;
    let body = client.get("/metrics").send().await.text();

    assert!(body.contains("my_orders_total"));
    assert!(body.contains("Total orders placed"));
    assert!(body.contains("my_orders_total 2"));
}

#[tokio::test]
async fn test_custom_metric_with_labels_appears_in_metrics_endpoint() {
    use rapina::prometheus::{IntCounterVec, Opts};

    let counter = IntCounterVec::new(
        Opts::new("orders_by_status_total", "Orders grouped by status"),
        &["status"],
    )
    .unwrap();
    counter.with_label_values(&["placed"]).inc();
    counter.with_label_values(&["placed"]).inc();
    counter.with_label_values(&["cancelled"]).inc();

    let app = Rapina::new()
        .with_introspection(false)
        .enable_metrics()
        .add_metric(counter);

    let client = TestClient::new(app).await;
    let body = client.get("/metrics").send().await.text();

    assert!(body.contains("orders_by_status_total"));
    assert!(body.contains(r#"status="placed""#));
    assert!(body.contains(r#"status="cancelled""#));
}

#[tokio::test]
async fn test_multiple_custom_metrics_all_appear_in_endpoint() {
    use rapina::prometheus::{IntCounter, IntGauge};

    let c1 = IntCounter::new("queue_processed_total", "Items processed").unwrap();
    let g1 = IntGauge::new("queue_depth", "Current queue depth").unwrap();
    c1.inc();
    g1.set(7);

    let app = Rapina::new()
        .with_introspection(false)
        .enable_metrics()
        .add_metric(c1)
        .add_metric(g1);

    let client = TestClient::new(app).await;
    let body = client.get("/metrics").send().await.text();

    assert!(body.contains("queue_processed_total"));
    assert!(body.contains("queue_depth 7"));
}

#[tokio::test]
async fn test_custom_metrics_coexist_with_builtin_metrics() {
    use rapina::prometheus::IntCounter;

    let counter = IntCounter::new("custom_coexist_total", "Custom metric").unwrap();

    let app = Rapina::new()
        .with_introspection(false)
        .enable_metrics()
        .add_metric(counter)
        .router(Router::new().route(http::Method::GET, "/ping", |_, _, _| async { "pong" }));

    let client = TestClient::new(app).await;
    client.get("/ping").send().await;

    let body = client.get("/metrics").send().await.text();

    // Built-in metrics still present
    assert!(body.contains("http_requests_total"));
    assert!(body.contains("http_request_duration_seconds"));
    assert!(body.contains("http_requests_in_flight"));

    // Custom metric also present
    assert!(body.contains("custom_coexist_total"));
}

// ── RAII guard for in-flight requests ────────────────────────

#[tokio::test]
async fn test_in_flight_metric_leak_on_client_disconnect() {
    // Build Rapina app with a slow endpoint
    let app = Rapina::new().with_metrics(true).router(Router::new().route(
        http::Method::GET,
        "/slow",
        |_, _, _| async move {
            // Sleep for 10 seconds, to simulate a slow DB query or long-running processing.
            tokio::time::sleep(Duration::from_secs(10)).await;
            StatusCode::OK
        },
    ));

    let client = TestClient::new(app).await;

    // Verify http_requests_in_flight metric starts at 1
    let metrics = client.get("/metrics").send().await.text();
    assert!(
        metrics.contains("http_requests_in_flight 1"),
        "in-flight metric should start at 1 (value 1 because of the current /metrics request)"
    );

    // Connect to the test server using a raw TCP socket
    let mut stream = TcpStream::connect(client.addr())
        .await
        .expect("Failed to connect via raw TCP");

    // Send a valid HTTP GET request to the /slow endpoint
    let request = "GET /slow HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n";
    stream
        .write_all(request.as_bytes())
        .await
        .expect("Failed to write to TCP stream");

    // Wait a bit to ensure Hyper starts processing the request
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify the metric incremented to 2
    let metrics = client.get("/metrics").send().await.text();
    assert!(
        metrics.contains("http_requests_in_flight 2"),
        "in-flight metric should increment to 2 while request is processing"
    );

    // THE CANCELLATION POINT, drops the TCP connection mid-flight
    drop(stream);

    // Wait a bit for the OS and Hyper to process the TCP close and our RAII guard to drop
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Verify http_requests_in_flight metric is back to 1
    let metrics = client.get("/metrics").send().await.text();
    assert!(
        metrics.contains("http_requests_in_flight 1"),
        "in-flight metric MUST return to 1 after client disconnects mid-request"
    );
}
