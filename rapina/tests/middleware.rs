//! Integration tests for middleware functionality.

use http::StatusCode;
#[cfg(feature = "compression")]
use rapina::middleware::CompressionConfig;
#[cfg(feature = "tower")]
use rapina::middleware::TowerLayerMiddleware;
use rapina::middleware::{
    BodyLimitMiddleware, CorsConfig, TRACE_ID_HEADER, TimeoutMiddleware, TraceIdMiddleware,
};
#[cfg(feature = "rate-limit")]
use rapina::middleware::{RateLimitConfig, RateLimitMiddleware};
use rapina::prelude::*;
use rapina::testing::TestClient;
use std::time::Duration;

#[tokio::test]
async fn test_middleware_execution() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "Hello!" }));

    let client = TestClient::new(app).await;
    let response = client.get("/").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    // TraceIdMiddleware should add x-trace-id header
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[tokio::test]
async fn test_trace_id_middleware_adds_header() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(Router::new().route(http::Method::GET, "/health", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/health").send().await;

    assert_eq!(response.status(), StatusCode::OK);

    let trace_id = response.headers().get(TRACE_ID_HEADER);
    assert!(trace_id.is_some());

    // Trace ID should be a valid UUID (36 characters)
    let trace_id_str = trace_id.unwrap().to_str().unwrap();
    assert_eq!(trace_id_str.len(), 36);
}

#[tokio::test]
async fn test_trace_id_unique_per_request() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    let response1 = client.get("/").send().await;
    let response2 = client.get("/").send().await;

    let trace_id1 = response1
        .headers()
        .get(TRACE_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap();
    let trace_id2 = response2
        .headers()
        .get(TRACE_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap();

    // Each request should have a unique trace ID
    assert_ne!(trace_id1, trace_id2);
}

#[tokio::test]
async fn test_timeout_middleware_passes_fast_request() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TimeoutMiddleware::new(Duration::from_secs(5)))
        .router(
            Router::new().route(http::Method::GET, "/fast", |_, _, _| async {
                "fast response"
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/fast").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text(), "fast response");
}

#[tokio::test]
async fn test_body_limit_middleware_allows_small_body() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(BodyLimitMiddleware::new(1024 * 1024)) // 1MB limit
        .router(
            Router::new().route(http::Method::POST, "/upload", |req, _, _| async move {
                use http_body_util::BodyExt;
                let body = req.into_body().collect().await.unwrap().to_bytes();
                format!("Received {} bytes", body.len())
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.post("/upload").body("small payload").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.text().contains("13 bytes")); // "small payload" is 13 bytes
}

#[tokio::test]
async fn test_multiple_middlewares() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .middleware(TimeoutMiddleware::new(Duration::from_secs(30)))
        .middleware(BodyLimitMiddleware::new(1024 * 1024))
        .router(Router::new().route(http::Method::GET, "/multi", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/multi").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text(), "ok");
    // TraceIdMiddleware should still add the header
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[tokio::test]
async fn test_middleware_order_trace_id_first() {
    // When TraceIdMiddleware is first, it should wrap the entire request
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .middleware(TimeoutMiddleware::default())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[tokio::test]
async fn test_middleware_with_post_request() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(
            Router::new().route(http::Method::POST, "/data", |req, _, _| async move {
                use http_body_util::BodyExt;
                let body = req.into_body().collect().await.unwrap().to_bytes();
                String::from_utf8_lossy(&body).to_string()
            }),
        );

    let client = TestClient::new(app).await;
    let response = client
        .post("/data")
        .json(&serde_json::json!({"key": "value"}))
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
    assert!(response.text().contains("key"));
}

#[tokio::test]
async fn test_default_timeout_middleware() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TimeoutMiddleware::default()) // 30 second default
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/").send().await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_default_body_limit_middleware() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(BodyLimitMiddleware::default()) // 1MB default
        .router(Router::new().route(http::Method::POST, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.post("/").body("test").send().await;

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn test_middleware_preserves_response_body() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(
            Router::new().route(http::Method::GET, "/json", |_, _, _| async {
                Json(serde_json::json!({
                    "status": "success",
                    "data": [1, 2, 3]
                }))
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/json").send().await;

    assert_eq!(response.status(), StatusCode::OK);

    let json: serde_json::Value = response.json();
    assert_eq!(json["status"], "success");
    assert_eq!(json["data"], serde_json::json!([1, 2, 3]));
}

#[tokio::test]
async fn test_middleware_with_error_response() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(
            Router::new().route(http::Method::GET, "/error", |_, _, _| async {
                Error::not_found("resource not found")
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/error").send().await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // Middleware should still add trace ID even for errors
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[tokio::test]
async fn test_middleware_with_404() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(Router::new().route(http::Method::GET, "/exists", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client.get("/does-not-exist").send().await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    // Middleware runs even for non-existent routes
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[tokio::test]
async fn test_cors_preflight_returns_204() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cors(CorsConfig::permissive())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    let response = client
        .request(http::Method::OPTIONS, "/")
        .header("Origin", "http://userapina.com")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_some()
    );
    assert!(
        response
            .headers()
            .get("access-control-allow-methods")
            .is_some()
    );
}

#[tokio::test]
async fn test_cors_rejects_disallowed_origin() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cors(CorsConfig::with_origins(vec![
            "http://userapina.com".to_string(),
        ]))
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client
        .request(http::Method::GET, "/")
        .header("Origin", "http://evil.com")
        .send()
        .await;

    // Request goes through but NO Access-Control-Allow-Origin header
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("access-control-allow-origin")
            .is_none()
    );
}

#[tokio::test]
async fn test_cors_allows_matching_origin() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cors(CorsConfig::with_origins(vec![
            "http://userapina.com".to_string(),
        ]))
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let response = client
        .request(http::Method::GET, "/")
        .header("Origin", "http://userapina.com")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    let origin_header = response.headers().get("access-control-allow-origin");
    assert!(origin_header.is_some());
    assert_eq!(
        origin_header.unwrap().to_str().unwrap(),
        "http://userapina.com"
    );
}

#[tokio::test]
async fn test_cors_permissive_returns_wildcard() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cors(CorsConfig::permissive())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    let response = client
        .request(http::Method::OPTIONS, "/")
        .header("Origin", "http://any.com")
        .send()
        .await;

    let origin_header = response.headers().get("access-control-allow-origin");
    assert_eq!(origin_header.unwrap().to_str().unwrap(), "*");
}

#[cfg(feature = "rate-limit")]
#[tokio::test]
async fn test_rate_limit_allows_under_limit() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_rate_limit(RateLimitConfig::new(100.0, 10)) // 10 burst
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    // Should allow requests under the burst limit
    for _ in 0..5 {
        let response = client.get("/").send().await;
        assert_eq!(response.status(), StatusCode::OK);
    }
}

#[cfg(feature = "rate-limit")]
#[tokio::test]
async fn test_rate_limit_returns_429_when_exceeded() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(RateLimitMiddleware::new(RateLimitConfig::new(1.0, 2))) // 2 burst
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    // First two requests allowed (burst)
    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);
    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);

    // Third request should be rate limited
    let response = client.get("/").send().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[cfg(feature = "rate-limit")]
#[tokio::test]
async fn test_rate_limit_includes_retry_after_header() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_rate_limit(RateLimitConfig::new(1.0, 1)) // 1 burst, 1 req/sec
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    // First request allowed
    assert_eq!(client.get("/").send().await.status(), StatusCode::OK);

    // Second request rate limited with Retry-After
    let response = client.get("/").send().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let retry_after = response.headers().get("retry-after");
    assert!(retry_after.is_some());

    let retry_secs: u64 = retry_after.unwrap().to_str().unwrap().parse().unwrap();
    assert!(retry_secs >= 1);
}

#[cfg(feature = "rate-limit")]
#[tokio::test]
async fn test_rate_limit_returns_json_error() {
    let app = Rapina::new()
        .with_introspection(false)
        .enable_rfc7807_errors()
        .rfc7807_base_uri("https://userapina.com/errors/")
        .with_rate_limit(RateLimitConfig::new(1.0, 1))
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    // Exhaust the limit
    client.get("/").send().await;

    // Check the error response body
    let response = client.get("/").send().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);

    let json: serde_json::Value = response.json();
    assert_eq!(json["type"], "https://userapina.com/errors/rate-limited");
    assert_eq!(json["title"], "Rate Limited");
    assert_eq!(json["detail"], "too many requests");
    assert!(json["trace_id"].is_string());
}

#[cfg(feature = "rate-limit")]
#[tokio::test]
async fn test_rate_limit_per_minute_convenience() {
    // Test the per_minute convenience constructor
    let app = Rapina::new()
        .with_introspection(false)
        .with_rate_limit(RateLimitConfig::per_minute(60)) // 1 req/sec, 60 burst
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;

    // Should allow 60 rapid requests (burst capacity)
    for i in 0..60 {
        let response = client.get("/").send().await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "Request {} should succeed",
            i + 1
        );
    }

    // 61st should be rate limited
    let response = client.get("/").send().await;
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn test_compression_gzip() {
    let large_body = "hello from rapina ".repeat(100);
    let body_clone = large_body.clone();

    let app = Rapina::new()
        .with_introspection(false)
        .with_compression(CompressionConfig::default())
        .router(Router::new().route(http::Method::GET, "/", move |_, _, _| {
            let body = body_clone.clone();
            async move { body }
        }));

    let client = TestClient::new(app).await;
    let response = client
        .get("/")
        .header("Accept-Encoding", "gzip")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get("content-encoding").unwrap(), "gzip");
    assert_eq!(response.headers().get("vary").unwrap(), "Accept-Encoding");
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn test_compression_skips_small_response() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_compression(CompressionConfig::default())
        .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "small" }));

    let client = TestClient::new(app).await;
    let response = client
        .get("/")
        .header("Accept-Encoding", "gzip")
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-encoding").is_none());
}

#[cfg(feature = "compression")]
#[tokio::test]
async fn test_compression_skips_without_accept_encoding() {
    let large_body = "hello from rapina ".repeat(100);
    let body_clone = large_body.clone();

    let app = Rapina::new()
        .with_introspection(false)
        .with_compression(CompressionConfig::default())
        .router(Router::new().route(http::Method::GET, "/", move |_, _, _| {
            let body = body_clone.clone();
            async move { body }
        }));

    let client = TestClient::new(app).await;
    let response = client.get("/").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-encoding").is_none());
}

#[tokio::test]
async fn test_trace_id_middleware_preserves_incoming_trace_id() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(Router::new().route(http::Method::GET, "/health", |_, _, _| async { "ok" }));

    let client = TestClient::new(app).await;
    let custom_trace_id = "my-custom-trace-id-123";

    let response = client
        .get("/health")
        .header("x-trace-id", custom_trace_id)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::OK);

    let header_value = response.headers().get(TRACE_ID_HEADER).unwrap();
    assert_eq!(header_value.to_str().unwrap(), custom_trace_id);
}

async fn trace_id_for_headers(headers: &[(&str, &str)]) -> String {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .router(
            Router::new().route(http::Method::GET, "/health", |req, _, _| async move {
                req.extensions()
                    .get::<rapina::context::RequestContext>()
                    .unwrap()
                    .trace_id()
                    .to_owned()
            }),
        );

    let client = TestClient::new(app).await;
    let mut request = client.get("/health");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = request.send().await;

    assert_eq!(response.status(), StatusCode::OK);
    let header_value = response
        .headers()
        .get(TRACE_ID_HEADER)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(response.text(), header_value);
    header_value
}

#[tokio::test]
async fn test_trace_id_middleware_accepts_max_length_trace_id() {
    let id = "a".repeat(128);
    assert_eq!(trace_id_for_headers(&[("x-trace-id", &id)]).await, id);
}

#[tokio::test]
async fn test_trace_id_middleware_ignores_oversized_trace_id() {
    let id = "a".repeat(129);
    let trace_id = trace_id_for_headers(&[("x-trace-id", &id)]).await;
    assert_ne!(trace_id, id);
    assert_eq!(trace_id.len(), 36);
}

#[tokio::test]
async fn test_trace_id_middleware_ignores_trace_id_with_invalid_chars() {
    for id in ["has space", "a/b", "a\"b", "{\"x\":1}"] {
        let trace_id = trace_id_for_headers(&[("x-trace-id", id)]).await;
        assert_ne!(trace_id, id);
        assert_eq!(trace_id.len(), 36);
    }
}

#[tokio::test]
async fn test_trace_id_middleware_ignores_empty_trace_id() {
    let trace_id = trace_id_for_headers(&[("x-trace-id", "")]).await;
    assert_eq!(trace_id.len(), 36);
}

#[tokio::test]
async fn test_trace_id_middleware_uses_x_request_id_when_trace_id_absent() {
    let id = "req-1.a_b:2";
    assert_eq!(trace_id_for_headers(&[("x-request-id", id)]).await, id);
}

#[tokio::test]
async fn test_trace_id_middleware_prefers_x_trace_id_over_x_request_id() {
    let trace_id = trace_id_for_headers(&[
        ("x-trace-id", "from-trace"),
        ("x-request-id", "from-request"),
    ])
    .await;
    assert_eq!(trace_id, "from-trace");
}

#[tokio::test]
async fn test_trace_id_middleware_ignores_invalid_x_request_id() {
    let id = "a".repeat(129);
    let trace_id = trace_id_for_headers(&[("x-request-id", &id)]).await;
    assert_ne!(trace_id, id);
    assert_eq!(trace_id.len(), 36);
}

#[tokio::test]
async fn test_trace_id_middleware_does_not_fall_back_when_trace_id_invalid() {
    let trace_id = trace_id_for_headers(&[
        ("x-trace-id", "has space"),
        ("x-request-id", "from-request"),
    ])
    .await;
    assert_ne!(trace_id, "from-request");
    assert_eq!(trace_id.len(), 36);
}

#[tokio::test]
async fn test_timeout_middleware_rejects_slow_handler() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TimeoutMiddleware::new(Duration::from_millis(50)))
        .router(
            Router::new().route(http::Method::GET, "/slow", |_, _, _| async {
                tokio::time::sleep(Duration::from_millis(200)).await;
                "should not reach"
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/slow").send().await;

    assert_eq!(response.status(), StatusCode::REQUEST_TIMEOUT);
}

#[tokio::test]
async fn test_body_limit_middleware_rejects_large_body() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(BodyLimitMiddleware::new(100)) // 100 bytes limit
        .router(
            Router::new().route(http::Method::POST, "/upload", |req, _, _| async move {
                use http_body_util::BodyExt;
                let body = req.into_body().collect().await.unwrap().to_bytes();
                format!("Received {} bytes", body.len())
            }),
        );

    let client = TestClient::new(app).await;
    let large_body = "x".repeat(200);
    let response = client
        .post("/upload")
        .header("content-length", "200")
        .body(large_body)
        .send()
        .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_body_limit_middleware_allows_no_content_length() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(BodyLimitMiddleware::new(100)) // 100 bytes limit
        .router(
            Router::new().route(http::Method::POST, "/upload", |req, _, _| async move {
                use http_body_util::BodyExt;
                let body = req.into_body().collect().await.unwrap().to_bytes();
                format!("Received {} bytes", body.len())
            }),
        );

    let client = TestClient::new(app).await;

    // Hyper automatically injects Content-Length: 200 when serializing the request                                                                                        │
    // because Full<Bytes> implements http_body::Body with a known size_hint().                                                                                            │
    // The middleware reads Content-Length and rejects it as over the 100 byte limit.
    let response = client.post("/upload").body("x".repeat(200)).send().await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[cfg(feature = "tower")]
#[tokio::test]
async fn test_tower_layer_with_rapina_middleware() {
    let app = Rapina::new()
        .with_introspection(false)
        .middleware(TraceIdMiddleware::new())
        .middleware(TowerLayerMiddleware::new(tower_layer::Identity::new()))
        .router(
            Router::new().route(http::Method::GET, "/mixed", |_, _, _| async {
                "tower + rapina"
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/mixed").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.text(), "tower + rapina");
    assert!(response.headers().get(TRACE_ID_HEADER).is_some());
}

#[cfg(feature = "tower")]
#[tokio::test]
async fn test_rapina_service_direct_construction() {
    use rapina::middleware::{MiddlewareStack, RapinaService};
    use tower_service::Service;

    let router = Router::new().route(http::Method::GET, "/direct", |_, _, _| async {
        "from service"
    });
    let state = rapina::state::AppState::new();
    let middlewares = MiddlewareStack::new();

    let mut svc = RapinaService::new(router, state, middlewares);

    let _cloned = svc.clone();

    let ready = std::future::poll_fn(|cx| svc.poll_ready(cx)).await;
    assert!(ready.is_ok());
}

#[cfg(feature = "tower")]
#[tokio::test]
async fn test_tower_concurrency_limit_integration() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tower::limit::ConcurrencyLimitLayer;

    let max_concurrent = Arc::new(AtomicUsize::new(0));
    let current = Arc::new(AtomicUsize::new(0));
    let max_c = max_concurrent.clone();
    let cur = current.clone();

    let app = Rapina::new()
        .with_introspection(false)
        .layer(ConcurrencyLimitLayer::new(1))
        .router(
            Router::new().route(http::Method::GET, "/slow", move |_, _, _| {
                let max_c = max_c.clone();
                let cur = cur.clone();
                async move {
                    let c = cur.fetch_add(1, Ordering::SeqCst) + 1;
                    max_c.fetch_max(c, Ordering::SeqCst);
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    cur.fetch_sub(1, Ordering::SeqCst);
                    "done"
                }
            }),
        );

    let client = TestClient::new(app).await;

    let (r1, r2) = tokio::join!(client.get("/slow").send(), client.get("/slow").send());

    assert_eq!(r1.status(), StatusCode::OK);
    assert_eq!(r2.status(), StatusCode::OK);
    assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
}
