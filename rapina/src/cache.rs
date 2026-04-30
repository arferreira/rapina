//! Response caching layer with pluggable backends.
//!
//! Provides middleware-based caching for GET requests with automatic
//! invalidation on mutations. Supports in-memory caching out of the box
//! and Redis via the `cache-redis` feature flag.
//!
//! # Quick Start
//!
//! ```ignore
//! use rapina::prelude::*;
//! use rapina::cache::CacheConfig;
//!
//! Rapina::new()
//!     .with_cache(CacheConfig::in_memory(1000)).await?
//!     .router(router)
//!     .listen("127.0.0.1:3000")
//!     .await
//! ```

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use dashmap::DashMap;
use http::{Response, header};
use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::{Body, Incoming};

use crate::context::RequestContext;
use crate::middleware::{BoxFuture, Middleware, Next};
use crate::response::{BoxBody, empty, full};

/// Internal header injected by the `#[cache(ttl = N)]` macro.
/// The middleware reads this to determine caching behavior, then strips it.
pub(crate) const CACHE_TTL_HEADER: &str = "x-rapina-cache-ttl";

/// Header added to responses indicating cache status.
pub const CACHE_STATUS_HEADER: &str = "x-cache";

/// How often to run cleanup (every N operations).
const CLEANUP_INTERVAL: u64 = 1000;

/// A boxed future for trait object compatibility.
type CacheFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A cached HTTP response.
#[derive(Clone, Debug)]
pub struct CachedResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Bytes,
}

/// Trait for cache storage backends.
///
/// Uses `BoxFuture` returns for `dyn CacheBackend` compatibility.
/// In-memory implementations return immediately; Redis is inherently async.
pub trait CacheBackend: Send + Sync + 'static {
    /// Retrieves a cached response by key.
    fn get(&self, key: &str) -> CacheFuture<'_, Option<CachedResponse>>;

    /// Stores a response with the given TTL.
    fn set(&self, key: &str, response: CachedResponse, ttl: Duration) -> CacheFuture<'_, ()>;

    /// Invalidates all entries whose key starts with the given prefix.
    fn invalidate_prefix(&self, prefix: &str) -> CacheFuture<'_, ()>;
}

struct CacheEntry {
    response: CachedResponse,
    expires_at: Instant,
    created_at: Instant,
}

/// In-memory cache using DashMap with TTL-based expiry and capacity limits.
pub struct InMemoryCache {
    entries: Arc<DashMap<String, CacheEntry>>,
    max_entries: usize,
    op_count: Arc<AtomicU64>,
}

impl InMemoryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(DashMap::new()),
            max_entries,
            op_count: Arc::new(AtomicU64::new(0)),
        }
    }

    fn maybe_cleanup(&self) {
        let count = self.op_count.fetch_add(1, Ordering::Relaxed);
        if count > 0 && count % CLEANUP_INTERVAL == 0 {
            self.cleanup_expired();
        }
    }

    fn cleanup_expired(&self) {
        let now = Instant::now();
        self.entries.retain(|_, entry| entry.expires_at > now);
    }

    fn evict_if_full(&self) {
        if self.entries.len() < self.max_entries {
            return;
        }

        // Evict expired first
        self.cleanup_expired();

        if self.entries.len() < self.max_entries {
            return;
        }

        // Evict oldest entry
        let oldest_key = self
            .entries
            .iter()
            .min_by_key(|entry| entry.value().created_at)
            .map(|entry| entry.key().clone());

        if let Some(key) = oldest_key {
            self.entries.remove(&key);
        }
    }
}

impl CacheBackend for InMemoryCache {
    fn get(&self, key: &str) -> CacheFuture<'_, Option<CachedResponse>> {
        self.maybe_cleanup();

        let result = self.entries.get(key).and_then(|entry| {
            if entry.expires_at > Instant::now() {
                Some(entry.response.clone())
            } else {
                None
            }
        });

        // Remove expired entry on access
        if result.is_none() {
            self.entries
                .remove_if(key, |_, entry| entry.expires_at <= Instant::now());
        }

        Box::pin(std::future::ready(result))
    }

    fn set(&self, key: &str, response: CachedResponse, ttl: Duration) -> CacheFuture<'_, ()> {
        self.maybe_cleanup();
        self.evict_if_full();

        let now = Instant::now();
        self.entries.insert(
            key.to_string(),
            CacheEntry {
                response,
                expires_at: now + ttl,
                created_at: now,
            },
        );

        Box::pin(std::future::ready(()))
    }

    fn invalidate_prefix(&self, prefix: &str) -> CacheFuture<'_, ()> {
        self.entries.retain(|key, _| !key.starts_with(prefix));

        Box::pin(std::future::ready(()))
    }
}

/// Configuration for the cache layer.
pub enum CacheConfig {
    /// In-memory cache with a maximum number of entries.
    InMemory { max_entries: usize },
    /// Redis-backed cache (requires `cache-redis` feature).
    #[cfg(feature = "cache-redis")]
    Redis { url: String },
}

impl CacheConfig {
    /// Creates an in-memory cache configuration.
    pub fn in_memory(max_entries: usize) -> Self {
        CacheConfig::InMemory { max_entries }
    }

    /// Creates a Redis cache configuration.
    #[cfg(feature = "cache-redis")]
    pub fn redis(url: &str) -> Self {
        CacheConfig::Redis {
            url: url.to_string(),
        }
    }

    /// Builds the cache backend from this configuration.
    pub async fn build(self) -> Result<Arc<dyn CacheBackend>, std::io::Error> {
        match self {
            CacheConfig::InMemory { max_entries } => Ok(Arc::new(InMemoryCache::new(max_entries))),
            #[cfg(feature = "cache-redis")]
            CacheConfig::Redis { url } => {
                let backend = crate::cache_redis::RedisCache::connect(&url)
                    .await
                    .map_err(|e| {
                        std::io::Error::other(format!("Redis connection failed: {}", e))
                    })?;
                Ok(Arc::new(backend))
            }
        }
    }
}

/// Cache middleware that intercepts requests and serves cached responses.
///
/// On GET requests: checks cache, returns hit if found, caches miss if
/// handler sets `x-rapina-cache-ttl` header (via `#[cache(ttl = N)]`).
///
/// On POST/PUT/DELETE with 2xx: auto-invalidates cached GET responses
/// matching the resource path prefix.
pub struct CacheMiddleware {
    backend: Arc<dyn CacheBackend>,
}

impl CacheMiddleware {
    pub fn new(backend: Arc<dyn CacheBackend>) -> Self {
        Self { backend }
    }
}

impl Middleware for CacheMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let method = req.method().clone();
            let path = req.uri().path().to_string();
            let query = req.uri().query().unwrap_or("").to_string();

            // Only cache GET requests
            if method == http::Method::GET {
                let cache_key = build_cache_key(&path, &query);

                // Check cache
                if let Some(cached) = self.backend.get(&cache_key).await {
                    return build_response_from_cache(cached, "HIT");
                }

                // Cache miss — run handler
                let response = next.run(req).await;

                // Check if handler wants caching
                if let Some(ttl) = extract_ttl_header(&response) {
                    // Streaming bodies cannot be captured as a single Bytes blob.
                    // A handler asking to cache one is a programming error: log
                    // a warning so the operator notices the #[cache] is silently
                    // being ignored, then pass the response through unchanged.
                    if response.body().size_hint().exact().is_none() {
                        tracing::warn!(
                            path = %path,
                            ttl,
                            "cache middleware: streaming response cannot be cached, \
                             passthrough. Remove #[cache] from this handler."
                        );
                        return response;
                    }
                    let (parts, body) = response.into_parts();
                    let body_bytes = match body.collect().await {
                        Ok(collected) => collected.to_bytes(),
                        Err(_) => {
                            return Response::builder()
                                .status(http::StatusCode::INTERNAL_SERVER_ERROR)
                                .body(empty())
                                .unwrap();
                        }
                    };

                    // Build CachedResponse
                    let cached = CachedResponse {
                        status: parts.status.as_u16(),
                        headers: parts
                            .headers
                            .iter()
                            .filter(|(name, _)| name.as_str() != CACHE_TTL_HEADER)
                            .map(|(name, value)| {
                                (name.to_string(), value.to_str().unwrap_or("").to_string())
                            })
                            .collect(),
                        body: body_bytes.clone(),
                    };

                    // Store in cache
                    self.backend
                        .set(&cache_key, cached, Duration::from_secs(ttl))
                        .await;

                    // Return response without the internal header, with MISS marker
                    let mut response = Response::from_parts(parts, full(body_bytes));
                    response.headers_mut().remove(CACHE_TTL_HEADER);
                    response
                        .headers_mut()
                        .insert(CACHE_STATUS_HEADER, http::HeaderValue::from_static("MISS"));
                    return response;
                }

                return response;
            }

            // Mutations: run handler first
            let response = next.run(req).await;

            // Auto-invalidate on successful mutations
            if is_mutation(&method) && response.status().is_success() {
                let prefix = build_invalidation_prefix(&path);
                self.backend.invalidate_prefix(&prefix).await;
            }

            response
        })
    }
}

fn build_cache_key(path: &str, query: &str) -> String {
    if query.is_empty() {
        format!("GET:{}", path)
    } else {
        // Sort query params for consistent keys
        let mut params: Vec<&str> = query.split('&').collect();
        params.sort();
        format!("GET:{}?{}", path, params.join("&"))
    }
}

fn build_invalidation_prefix(path: &str) -> String {
    // /users/123 -> invalidate GET:/users
    // /users -> invalidate GET:/users
    let base = path
        .rfind('/')
        .filter(|&i| i > 0)
        .map(|i| &path[..i])
        .unwrap_or(path);
    format!("GET:{}", base)
}

fn is_mutation(method: &http::Method) -> bool {
    matches!(
        *method,
        http::Method::POST | http::Method::PUT | http::Method::DELETE | http::Method::PATCH
    )
}

fn extract_ttl_header(response: &Response<BoxBody>) -> Option<u64> {
    response
        .headers()
        .get(CACHE_TTL_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
}

fn build_response_from_cache(cached: CachedResponse, status: &'static str) -> Response<BoxBody> {
    let mut builder = Response::builder().status(cached.status);

    for (name, value) in &cached.headers {
        if let (Ok(name), Ok(value)) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            header::HeaderValue::from_str(value),
        ) {
            builder = builder.header(name, value);
        }
    }

    let mut response = builder.body(full(cached.body)).unwrap();

    response
        .headers_mut()
        .insert(CACHE_STATUS_HEADER, http::HeaderValue::from_static(status));

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_in_memory_cache_set_and_get() {
        let cache = InMemoryCache::new(100);
        let response = CachedResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body: Bytes::from(r#"{"ok":true}"#),
        };

        cache
            .set("key1", response.clone(), Duration::from_secs(60))
            .await;

        let result = cache.get("key1").await;
        assert!(result.is_some());

        let cached = result.unwrap();
        assert_eq!(cached.status, 200);
        assert_eq!(cached.body, Bytes::from(r#"{"ok":true}"#));
    }

    #[tokio::test]
    async fn test_in_memory_cache_miss() {
        let cache = InMemoryCache::new(100);
        let result = cache.get("nonexistent").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_cache_ttl_expiry() {
        let cache = InMemoryCache::new(100);
        let response = CachedResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from("data"),
        };

        // Insert with very short TTL
        cache.set("key1", response, Duration::from_millis(1)).await;

        // Wait for expiry
        tokio::time::sleep(Duration::from_millis(10)).await;

        let result = cache.get("key1").await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_in_memory_cache_invalidate_prefix() {
        let cache = InMemoryCache::new(100);
        let response = CachedResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from("data"),
        };

        cache
            .set("GET:/users", response.clone(), Duration::from_secs(60))
            .await;
        cache
            .set("GET:/users/1", response.clone(), Duration::from_secs(60))
            .await;
        cache
            .set("GET:/posts", response.clone(), Duration::from_secs(60))
            .await;

        cache.invalidate_prefix("GET:/users").await;

        assert!(cache.get("GET:/users").await.is_none());
        assert!(cache.get("GET:/users/1").await.is_none());
        assert!(cache.get("GET:/posts").await.is_some());
    }

    #[tokio::test]
    async fn test_in_memory_cache_max_entries_eviction() {
        let cache = InMemoryCache::new(2);
        let response = CachedResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from("data"),
        };

        cache
            .set("key1", response.clone(), Duration::from_secs(60))
            .await;
        cache
            .set("key2", response.clone(), Duration::from_secs(60))
            .await;

        // This should evict the oldest (key1)
        cache
            .set("key3", response.clone(), Duration::from_secs(60))
            .await;

        assert!(cache.get("key1").await.is_none());
        assert!(cache.get("key2").await.is_some());
        assert!(cache.get("key3").await.is_some());
    }

    #[tokio::test]
    async fn test_in_memory_cache_cleanup_expired() {
        let cache = InMemoryCache::new(100);
        let response = CachedResponse {
            status: 200,
            headers: vec![],
            body: Bytes::from("data"),
        };

        cache
            .set("expired", response.clone(), Duration::from_millis(1))
            .await;
        cache
            .set("fresh", response.clone(), Duration::from_secs(60))
            .await;

        tokio::time::sleep(Duration::from_millis(10)).await;

        cache.cleanup_expired();

        assert_eq!(cache.entries.len(), 1);
        assert!(cache.entries.get("fresh").is_some());
    }

    #[test]
    fn test_build_cache_key_no_query() {
        assert_eq!(build_cache_key("/users", ""), "GET:/users");
    }

    #[test]
    fn test_build_cache_key_with_query() {
        let key = build_cache_key("/users", "page=1&sort=name");
        assert_eq!(key, "GET:/users?page=1&sort=name");
    }

    #[test]
    fn test_build_cache_key_sorts_query_params() {
        let key1 = build_cache_key("/users", "sort=name&page=1");
        let key2 = build_cache_key("/users", "page=1&sort=name");
        assert_eq!(key1, key2);
    }

    #[test]
    fn test_build_invalidation_prefix() {
        assert_eq!(build_invalidation_prefix("/users/123"), "GET:/users");
        assert_eq!(build_invalidation_prefix("/users"), "GET:/users");
        assert_eq!(build_invalidation_prefix("/"), "GET:/");
    }

    #[test]
    fn test_is_mutation() {
        assert!(is_mutation(&http::Method::POST));
        assert!(is_mutation(&http::Method::PUT));
        assert!(is_mutation(&http::Method::DELETE));
        assert!(is_mutation(&http::Method::PATCH));
        assert!(!is_mutation(&http::Method::GET));
        assert!(!is_mutation(&http::Method::HEAD));
    }

    #[test]
    fn test_cache_config_in_memory() {
        let config = CacheConfig::in_memory(500);
        assert!(matches!(config, CacheConfig::InMemory { max_entries: 500 }));
    }

    #[test]
    fn test_build_response_from_cache() {
        let cached = CachedResponse {
            status: 200,
            headers: vec![("content-type".to_string(), "text/plain".to_string())],
            body: Bytes::from("hello"),
        };

        let response = build_response_from_cache(cached, "HIT");
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers().get(CACHE_STATUS_HEADER).unwrap(), "HIT");
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/plain"
        );
    }
}
