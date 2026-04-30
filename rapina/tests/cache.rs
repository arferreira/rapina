//! Integration tests for the caching layer.

use http::StatusCode;
use rapina::cache::{CACHE_STATUS_HEADER, CacheConfig};
use rapina::prelude::*;
use rapina::testing::TestClient;

#[tokio::test]
async fn test_cache_miss_then_hit() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::GET, "/data", |_, _, _| async {
                let mut response = http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .body(rapina::response::full(bytes::Bytes::from(
                        r#"{"value":42}"#,
                    )))
                    .unwrap();
                response
                    .headers_mut()
                    .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("60"));
                response
            }),
        );

    let client = TestClient::new(app).await;

    // First request: MISS
    let response = client.get("/data").send().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "MISS");
    assert_eq!(response.text(), r#"{"value":42}"#);

    // Second request: HIT
    let response = client.get("/data").send().await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "HIT");
    assert_eq!(response.text(), r#"{"value":42}"#);
}

#[tokio::test]
async fn test_cache_strips_internal_ttl_header() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::GET, "/data", |_, _, _| async {
                let mut response = http::Response::builder()
                    .status(StatusCode::OK)
                    .body(rapina::response::full(bytes::Bytes::from("ok")))
                    .unwrap();
                response
                    .headers_mut()
                    .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("30"));
                response
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/data").send().await;

    // Internal header should be stripped
    assert!(response.headers().get("x-rapina-cache-ttl").is_none());
}

#[tokio::test]
async fn test_cache_does_not_cache_without_ttl_header() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    let counter = Arc::new(AtomicU32::new(0));
    let counter_clone = counter.clone();

    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::GET, "/data", move |_, _, _| {
                let counter = counter_clone.clone();
                async move {
                    let n = counter.fetch_add(1, Ordering::Relaxed);
                    format!("call {}", n)
                }
            }),
        );

    let client = TestClient::new(app).await;

    let r1 = client.get("/data").send().await;
    let r2 = client.get("/data").send().await;

    // Without TTL header, no caching — handler runs each time
    assert!(r1.headers().get(CACHE_STATUS_HEADER).is_none());
    assert!(r2.headers().get(CACHE_STATUS_HEADER).is_none());
    assert_ne!(r1.text(), r2.text());
}

#[tokio::test]
async fn test_mutation_invalidates_cache() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new()
                .route(http::Method::GET, "/items", |_, _, _| async {
                    let mut response = http::Response::builder()
                        .status(StatusCode::OK)
                        .body(rapina::response::full(bytes::Bytes::from("items")))
                        .unwrap();
                    response
                        .headers_mut()
                        .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("60"));
                    response
                })
                .route(http::Method::POST, "/items", |_, _, _| async {
                    StatusCode::CREATED
                }),
        );

    let client = TestClient::new(app).await;

    // Populate cache
    let response = client.get("/items").send().await;
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "MISS");

    // Verify cache hit
    let response = client.get("/items").send().await;
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "HIT");

    // Mutation should invalidate
    let response = client.post("/items").send().await;
    assert_eq!(response.status(), StatusCode::CREATED);

    // Should be a miss again
    let response = client.get("/items").send().await;
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "MISS");
}

#[tokio::test]
async fn test_cache_preserves_response_headers() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::GET, "/data", |_, _, _| async {
                let mut response = http::Response::builder()
                    .status(StatusCode::OK)
                    .header("content-type", "application/json")
                    .header("x-custom", "preserved")
                    .body(rapina::response::full(bytes::Bytes::from("{}")))
                    .unwrap();
                response
                    .headers_mut()
                    .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("60"));
                response
            }),
        );

    let client = TestClient::new(app).await;

    // Populate cache
    client.get("/data").send().await;

    // Cache hit should preserve original headers
    let response = client.get("/data").send().await;
    assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "HIT");
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "application/json"
    );
    assert_eq!(response.headers().get("x-custom").unwrap(), "preserved");
}

#[tokio::test]
async fn test_cache_only_caches_get() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::POST, "/data", |_, _, _| async {
                let mut response = http::Response::builder()
                    .status(StatusCode::CREATED)
                    .body(rapina::response::full(bytes::Bytes::from("created")))
                    .unwrap();
                response
                    .headers_mut()
                    .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("60"));
                response
            }),
        );

    let client = TestClient::new(app).await;

    // POST requests should not be cached even with TTL header
    let response = client.post("/data").send().await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(response.headers().get(CACHE_STATUS_HEADER).is_none());
}

#[tokio::test]
async fn test_cache_query_params_affect_key() {
    let app = Rapina::new()
        .with_introspection(false)
        .with_cache(CacheConfig::in_memory(100))
        .await
        .unwrap()
        .router(
            Router::new().route(http::Method::GET, "/search", |req, _, _| async move {
                let query = req.uri().query().unwrap_or("none").to_string();
                let mut response = http::Response::builder()
                    .status(StatusCode::OK)
                    .body(rapina::response::full(bytes::Bytes::from(query)))
                    .unwrap();
                response
                    .headers_mut()
                    .insert("x-rapina-cache-ttl", http::HeaderValue::from_static("60"));
                response
            }),
        );

    let client = TestClient::new(app).await;

    // Different query params should produce different cache keys
    let r1 = client.get("/search?q=rust").send().await;
    assert_eq!(r1.headers().get(CACHE_STATUS_HEADER).unwrap(), "MISS");

    let r2 = client.get("/search?q=python").send().await;
    assert_eq!(r2.headers().get(CACHE_STATUS_HEADER).unwrap(), "MISS");

    // Same query should hit
    let r3 = client.get("/search?q=rust").send().await;
    assert_eq!(r3.headers().get(CACHE_STATUS_HEADER).unwrap(), "HIT");
    assert_eq!(r3.text(), "q=rust");
}
