+++
title = "Caching"
description = "Response caching with in-memory and Redis backends"
weight = 8
date = 2026-02-27
+++

Rapina includes a response caching layer that stores GET responses and automatically invalidates them when data changes. Two backends are available: **in-memory** (default, single instance) and **Redis** (multi-instance deployments). Routes opt in to caching with a `#[cache(ttl = N)]` attribute — everything else passes through untouched.

## Quick Start

Enable caching with a single builder call and mark the routes you want cached:

```rust
use rapina::prelude::*;
use rapina::cache::CacheConfig;

#[get("/products")]
#[cache(ttl = 60)]
async fn list_products(db: Db) -> Result<Json<Vec<Product>>> {
    let products = Product::find().all(db.conn()).await?;
    Ok(Json(products))
}

#[post("/products")]
async fn create_product(db: Db, body: Json<NewProduct>) -> Result<Json<Product>> {
    let product = body.into_inner().insert(db.conn()).await?;  // into_inner() — ownership needed for insert
    Ok(Json(product))
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .with_cache(CacheConfig::in_memory(1000)).await?
        .discover()
        .listen("127.0.0.1:3000")
        .await
}
```

The `ttl` parameter is in seconds. Only GET requests are cached. `create_product` doesn't need `#[cache]` — successful POSTs automatically invalidate the related GET cache.

---

## How It Works

The cache middleware intercepts every request. What it does depends on the HTTP method:

**GET requests** — the middleware computes a cache key from the path and sorted query parameters. If a cached entry exists and hasn't expired, it's returned immediately without touching your handler. On a miss, the handler runs. If the handler's route has `#[cache(ttl = N)]`, the response is stored with that TTL and returned with `x-cache: MISS`. The next identical GET returns `x-cache: HIT`.

**Mutations (POST/PUT/DELETE/PATCH)** — the handler always runs. If the response is 2xx, the middleware invalidates cached entries matching the resource's path prefix. A `POST /products` clears `GET:/products`, `GET:/products?page=1`, etc. A failed mutation (4xx, 5xx) leaves the cache alone.

**Everything else** — passes through. HEAD, OPTIONS, and other methods are unaffected.

### Auto-Invalidation

Invalidation is prefix-based. The middleware strips the last path segment to find the resource collection, then removes every cached key that starts with that prefix:

```rust
// Cached at GET:/products
#[get("/products")]
#[cache(ttl = 300)]
async fn list_products(db: Db) -> Result<Json<Vec<Product>>> { ... }

// Cached at GET:/products/42
#[get("/products/:id")]
#[cache(ttl = 300)]
async fn get_product(db: Db, id: Path<i32>) -> Result<Json<Product>> { ... }

// POST /products → invalidates GET:/products and GET:/products/42
#[post("/products")]
async fn create_product(db: Db, body: Json<NewProduct>) -> Result<Json<Product>> { ... }

// DELETE /products/42 → invalidates GET:/products and GET:/products/42
#[delete("/products/:id")]
async fn delete_product(db: Db, id: Path<i32>) -> Result<StatusCode> { ... }
```

The invalidation is conservative — it clears more than necessary rather than risk stale data. A `DELETE /products/42` invalidates the entire `/products` collection, not just the single item.

### Cache Key Format

Keys are built from the HTTP method, path, and sorted query parameters:

```
GET:/products                      # /products
GET:/products?page=1&sort=name     # /products?sort=name&page=1 (sorted)
```

Query parameter order doesn't matter — `?page=1&sort=name` and `?sort=name&page=1` produce the same cache key. This prevents cache fragmentation from clients that serialize params in different orders.

The key does **not** include request headers. If your endpoint returns different content based on `Accept` or `Authorization`, don't cache it (or build the differentiation into query params).

---

## Configuration

### In-Memory

Uses lock-free concurrent storage (DashMap). Each entry has its own TTL. When the max entry count is reached, the eviction strategy is: remove expired entries first, then evict the oldest. Periodic cleanup runs every 1000 cache operations to prevent unbounded memory growth from expired-but-not-yet-evicted entries.

```rust
CacheConfig::in_memory(1000)  // up to 1000 cached responses
```

Good for single-instance deployments and development. Zero external dependencies. The cache lives in process memory and is lost on restart.

### Redis

For multi-instance deployments where all instances need to share the same cache state. Requires the `cache-redis` feature:

```toml
[dependencies]
rapina = { version = "0.12.0", features = ["cache-redis"] }
```

```rust
use rapina::cache::CacheConfig;

Rapina::new()
    .with_cache(CacheConfig::redis("redis://localhost:6379")).await?
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

| Detail | Value |
|--------|-------|
| Connection | Multiplexed async (single TCP connection, many concurrent commands) |
| Serialization | JSON via `serde_json` |
| TTL | Native Redis `SET EX` — Redis handles expiry, no application-side timers |
| Key prefix | `rapina:` by default |
| Invalidation | `SCAN` + `DEL` pattern matching |

Keys in Redis look like `rapina:GET:/products?page=1&sort=name`. The `rapina:` prefix prevents collisions if you share a Redis instance with other applications.

#### Key prefix customization

If you're running multiple Rapina services against the same Redis, use `RedisCache` directly to set a custom prefix:

```rust
use rapina::cache::CacheMiddleware;
use rapina::cache_redis::RedisCache;

let backend = RedisCache::connect("redis://localhost:6379", None)
    .await?
    .with_prefix("myapp:");

Rapina::new()
    .middleware(CacheMiddleware::new(std::sync::Arc::new(backend)))
    .discover()
    .listen("127.0.0.1:3000")
    .await
```


#### TLS Certificates

If you also want to use a certificate alongside the redis, pass it after the url like this:

```rust
CacheConfig::redis_with_tls("rediss://localhost", RedisTlsConfig {
    ca_cert: Some(std::fs::read("ca.pem")?),
    client_cert: Some(std::fs::read("client.pem")?),
    client_key: Some(std::fs::read("client.key")?),
})
```
---
## Response Headers

Every cached GET response includes an `x-cache` header:

| Header | Value | Meaning |
|--------|-------|---------|
| `x-cache` | `HIT` | Response served from cache |
| `x-cache` | `MISS` | Response generated by handler, now cached |

Routes without `#[cache]` don't produce this header. The internal `x-rapina-cache-ttl` header used for communication between the macro and middleware is always stripped before the response reaches the client.

---

## Middleware Ordering

Cache should be registered **before** compression. If cache runs after compression, you'd store gzip-encoded bytes and serve them to clients that didn't ask for gzip.

Cache should be registered **after** CORS and rate limiting. There's no point caching a rate-limited 429 or serving a cached response without CORS headers.

```rust
use rapina::middleware::{
    CorsConfig, CompressionConfig, TraceIdMiddleware, RateLimitConfig,
};
use rapina::cache::CacheConfig;

Rapina::new()
    .middleware(TraceIdMiddleware::new())
    .with_cors(CorsConfig::permissive())
    .with_rate_limit(RateLimitConfig::per_minute(60))
    .with_cache(CacheConfig::in_memory(1000)).await?
    .with_compression(CompressionConfig::default())
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

| Position | Middleware | Reason |
|----------|-----------|--------|
| 1 | Trace ID | All downstream logs carry the request ID |
| 2 | CORS | Preflights answered before consuming cache/quota |
| 3 | Rate limit | Blocked clients never touch the cache |
| 4 | **Cache** | Hits returned before handler runs |
| 5 | Compression | Compresses the final (uncached) response body |
| Last | Auth | Framework-managed, always appended at `listen()` |

---

## Custom Cache Backend

The `CacheBackend` trait lets you plug in your own storage. Implement three methods:

```rust
use rapina::cache::{CacheBackend, CachedResponse};
use std::pin::Pin;
use std::future::Future;
use std::time::Duration;

type CacheFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub struct MyCache { /* ... */ }

impl CacheBackend for MyCache {
    fn get(&self, key: &str) -> CacheFuture<'_, Option<CachedResponse>> {
        Box::pin(async move {
            // Look up `key`, return None on miss
            todo!()
        })
    }

    fn set(&self, key: &str, response: CachedResponse, ttl: Duration) -> CacheFuture<'_, ()> {
        Box::pin(async move {
            // Store `response` under `key` with the given TTL
            todo!()
        })
    }

    fn invalidate_prefix(&self, prefix: &str) -> CacheFuture<'_, ()> {
        Box::pin(async move {
            // Remove all entries whose key starts with `prefix`
            todo!()
        })
    }
}
```

`CachedResponse` stores the status code, headers, and body bytes. The body uses `bytes::Bytes` which is reference-counted — cloning is cheap.

Register your backend directly via the middleware:

```rust
use rapina::cache::CacheMiddleware;
use std::sync::Arc;

let backend = Arc::new(MyCache::new());

Rapina::new()
    .middleware(CacheMiddleware::new(backend))
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

---

## What Not to Cache

The cache key is `method + path + sorted query params`. It does not include headers, cookies, or the request body. This means caching is safe for public, anonymous, read-only endpoints but requires care in other situations.

**User-specific responses** — if `GET /dashboard` returns different data per user (based on the `Authorization` header), caching it would serve user A's data to user B. Either don't cache these routes, or encode the user identity in the URL (e.g., `/users/123/dashboard`).

**Content negotiation** — if your endpoint checks `Accept: application/xml` vs `application/json` and returns different formats, the cache doesn't distinguish between them. Pick one format per endpoint.

**Side effects** — a GET handler that increments a view counter or sends analytics events will only fire on cache misses. If the side effect matters on every request, don't cache.

**Large responses** — cached responses live in memory (in-memory backend) or get serialized to JSON (Redis). A 50MB response cached 100 times across different query params will cost 5GB. Set `max_entries` conservatively and keep cached payloads reasonable.

---

## Full Example

A realistic setup with database, caching, auth, and both cached and uncached routes:

```rust
use rapina::prelude::*;
use rapina::cache::CacheConfig;
use rapina::database::{DatabaseConfig, Db};
use rapina::middleware::{TraceIdMiddleware, CompressionConfig, CorsConfig};

#[get("/products")]
#[cache(ttl = 60)]
async fn list_products(db: Db) -> Result<Json<Vec<Product>>> {
    let products = Product::find().all(db.conn()).await?;
    Ok(Json(products))
}

#[get("/products/:id")]
#[cache(ttl = 120)]
async fn get_product(db: Db, id: Path<i32>) -> Result<Json<Product>> {
    let product = Product::find_by_id(*id)
        .one(db.conn())
        .await?
        .ok_or(Error::not_found("product not found"))?;
    Ok(Json(product))
}

#[post("/products")]
async fn create_product(db: Db, body: Validated<Json<NewProduct>>) -> Result<Json<Product>> {
    let product = body.into_inner().into_inner().insert(db.conn()).await?;  // ownership needed for insert
    Ok(Json(product))
}

// Not cached — returns user-specific data
#[get("/me")]
async fn me(user: CurrentUser) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "user_id": user.id }))
}

#[public]
#[get("/health")]
async fn health() -> &'static str {
    "ok"
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    load_dotenv();

    let auth = AuthConfig::from_env().expect("JWT_SECRET required");
    let db = DatabaseConfig::from_env()?;

    Rapina::new()
        .middleware(TraceIdMiddleware::new())
        .with_cors(CorsConfig::permissive())
        .with_cache(CacheConfig::in_memory(500)).await?
        .with_compression(CompressionConfig::default())
        .with_auth(auth)
        .with_database(db).await?
        .discover()
        .listen("127.0.0.1:3000")
        .await
}
```

`list_products` and `get_product` are cached. `create_product` invalidates both on success. `me` is not cached because it depends on the authenticated user. `health` is public and uncached.
