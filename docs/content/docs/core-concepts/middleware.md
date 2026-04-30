+++
title = "Middleware"
description = "Request logging, rate limiting, compression, CORS, timeout, and custom middleware"
weight = 7
date = 2026-02-23
+++

Middleware intercepts requests before they reach your handler and responses before they are sent to the client. Each piece in the chain can inspect, modify, or short-circuit the request lifecycle.

## Rate Limiting

> **Feature flag:** Rate limiting requires the `rate-limit` feature, which is enabled by default. To opt out, use `default-features = false` in your `Cargo.toml`.

Protects your API from abuse using the **token bucket** algorithm. Each client gets a quota of tokens that refills continuously at the configured rate.

```rust
Rapina::new()
    .with_rate_limit(RateLimitConfig::per_minute(60))
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

`RateLimitConfig` is available from `use rapina::prelude::*`.

| Constructor | Description |
|-------------|-------------|
| `RateLimitConfig::new(rps, burst)` | Requests per second with burst capacity |
| `RateLimitConfig::per_minute(n)` | Shorthand for `n` requests per minute |

```rust
RateLimitConfig::new(2.0, 10)     // 2 req/s, burst of 10
RateLimitConfig::per_minute(120)  // burst of 120, refills at 2 req/s
```

When the limit is exceeded, Rapina responds `429 Too Many Requests` with a `Retry-After` header.

### Key extraction

Limits are applied per client IP by default, read from `X-Forwarded-For` (leftmost entry) then `X-Real-IP`, falling back to `"unknown"`. Use `KeyExtractor::Custom` to limit by any other key:

```rust
use std::sync::Arc;

let config = RateLimitConfig::per_minute(100)
    .with_key_extractor(KeyExtractor::Custom(Arc::new(|req| {
        req.headers()
            .get("x-user-id")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("anonymous")
            .to_string()
    })));
```

New clients start with a full burst. Inactive buckets (idle for more than 10 minutes) are cleaned up every 1000 requests.

---

## Response Compression

Compresses responses automatically, negotiating the algorithm via `Accept-Encoding`. Gzip takes priority over deflate.

```rust
use rapina::middleware::CompressionConfig;

Rapina::new()
    .with_compression(CompressionConfig::default())
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

| Field | Default | Description |
|-------|---------|-------------|
| `min_size` | `1024` | Minimum body size in bytes to compress |
| `level` | `6` | Compression level 0–9 |

```rust
CompressionConfig::default()
CompressionConfig::new(512, 9)  // min 512 bytes, maximum compression
```

Compression is skipped when the client does not send `Accept-Encoding: gzip` or `deflate`, the response already has a `Content-Encoding` header, the `Content-Type` is not compressible (e.g. `image/png`), or (for buffered bodies) the body is smaller than `min_size`. `Vary: Accept-Encoding` is added automatically for correct proxy caching.

### Streaming bodies

`StreamResponse` and `SseResponse` (see [Streaming & SSE](@/docs/core-concepts/streaming.md)) are handled differently:

- **`text/event-stream`**: never compressed. Gzip across event boundaries breaks SSE framing at proxies.
- **Other streaming bodies**: compressed per-chunk with a persistent gzip/deflate encoder. The `min_size` and "compression not worth it" guards don't apply, since the total size isn't known in advance. Streaming bodies always get compressed when the client accepts it. `Content-Length` is removed; the response uses chunked transfer encoding.

---

## CORS

Controls which origins can access your API.

```rust
use rapina::middleware::CorsConfig;

Rapina::new()
    .with_cors(CorsConfig::permissive())
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

| Constructor | Use case |
|-------------|----------|
| `CorsConfig::permissive()` | Development — allows any origin, method, and header |
| `CorsConfig::with_origins(vec![...])` | Production — restricts to specific origins |

```rust
// Development
CorsConfig::permissive()

// Production
CorsConfig::with_origins(vec![
    "https://app.example.com".to_string(),
    "https://admin.example.com".to_string(),
])
```

`with_origins` defaults to methods `GET POST PUT PATCH DELETE OPTIONS` and headers `Accept Authorization`. `OPTIONS` preflight requests return `204 No Content` and never reach your handler. `Vary: Origin` is added to every response.

### Advanced configuration

```rust
use rapina::middleware::{CorsConfig, AllowedOrigins, AllowedMethods, AllowedHeaders};
use http::{Method, header};

let cors = CorsConfig {
    allowed_origins: AllowedOrigins::Exact(vec![
        "https://app.example.com".to_string(),
    ]),
    allowed_methods: AllowedMethods::List(vec![
        Method::GET,
        Method::POST,
        Method::DELETE,
    ]),
    allowed_headers: AllowedHeaders::List(vec![
        header::ACCEPT,
        header::AUTHORIZATION,
        header::CONTENT_TYPE,
    ]),
};
```

---

## Request Logging

Structured request/response logging built on `tracing`. By default it logs method, path, status, and duration at INFO level. Turn on extra fields with `RequestLogConfig`.

```rust
use rapina::prelude::*;

// Zero-config — method, path, status, duration only
Rapina::new()
    .middleware(RequestLogMiddleware::new())
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

For richer output, use the convenience builder or pass a config directly:

```rust
use rapina::prelude::*;

// Everything on, sensitive headers redacted
Rapina::new()
    .with_request_log(RequestLogConfig::verbose())
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

`RequestLogConfig::verbose()` enables header logging, query parameter logging, and body size logging, with these headers redacted by default: `authorization`, `proxy-authorization`, `cookie`, `set-cookie`, `x-api-key`.

| Field | Default | Verbose |
|-------|---------|---------|
| `log_headers` | `false` | `true` |
| `log_query_params` | `false` | `true` |
| `log_body_size` | `false` | `true` |
| `redacted_headers` | `[]` | 5 headers |

Toggle individual flags or add custom redactions:

```rust
let config = RequestLogConfig::default()
    .log_headers(true)
    .log_body_size(true)
    .redact_header("x-custom-secret");

Rapina::new()
    .with_request_log(config)
```

Redacted header values appear as `[REDACTED]` in the logs. Matching is case-insensitive. All extra fields are emitted as structured tracing fields, so they flow through whatever subscriber you configure (JSON, logfmt, etc.).

---

## Timeout, Body Limit, and Trace ID

These middleware ship with Rapina but are not active by default. Register them with `.middleware()`:

```rust
use rapina::middleware::{TimeoutMiddleware, BodyLimitMiddleware, TraceIdMiddleware};
use std::time::Duration;

Rapina::new()
    .middleware(TraceIdMiddleware::new())
    .middleware(TimeoutMiddleware::new(Duration::from_secs(30)))
    .middleware(BodyLimitMiddleware::new(1024 * 1024)) // 1 MB
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

### Timeout

Cancels requests that exceed the configured duration and responds `500 Internal Server Error`.

```rust
TimeoutMiddleware::default()                      // 30 seconds
TimeoutMiddleware::new(Duration::from_secs(10))  // custom
```

### Body limit

Rejects requests whose `Content-Length` exceeds the limit with `400 Bad Request`. Requests without a `Content-Length` header are not checked.

```rust
BodyLimitMiddleware::default()              // 1 MB
BodyLimitMiddleware::new(5 * 1024 * 1024)  // 5 MB
```

### Trace ID

Assigns a unique identifier to every request for distributed tracing.

- Accepts an incoming `x-trace-id` header from upstream; otherwise generates a UUID v4
- Echoes the trace ID back in the response header `x-trace-id`

Access it in a handler via the `Context` extractor:

```rust
#[get("/")]
async fn handler(ctx: Context) -> String {
    format!("trace: {}", ctx.trace_id())
}
```

---

## Custom Middleware

Implement the `Middleware` trait. Call `next.run(req)` to continue the chain, or return a response early to short-circuit it.

```rust
use rapina::middleware::BoxFuture;
use rapina::response::BoxBody;
use hyper::{Request, Response, body::Incoming};

pub struct ApiKeyMiddleware {
    valid_keys: Vec<String>,
}

impl Middleware for ApiKeyMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let key = req
                .headers()
                .get("x-api-key")
                .and_then(|v| v.to_str().ok());

            match key {
                Some(k) if self.valid_keys.contains(&k.to_string()) => next.run(req).await,
                _ => Error::unauthorized("missing or invalid API key").into_response(),
            }
        })
    }
}
```

Register with `.middleware()`:

```rust
Rapina::new()
    .middleware(ApiKeyMiddleware {
        valid_keys: vec!["secret-123".to_string()],
    })
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

### Injecting data into the request

Middleware can insert typed values into request extensions. Implement `FromRequestParts` to receive them in a handler:

```rust
use rapina::middleware::BoxFuture;
use rapina::response::BoxBody;
use rapina::extract::{FromRequestParts, PathParams};
use rapina::state::AppState;
use hyper::{Request, Response, body::Incoming};
use http::request::Parts;
use std::sync::Arc;

#[derive(Clone)]
pub struct TenantId(pub String);

impl FromRequestParts for TenantId {
    async fn from_request_parts(
        parts: &Parts,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        parts
            .extensions
            .get::<TenantId>()
            .cloned()
            .ok_or_else(|| Error::internal("TenantId not injected by middleware"))
    }
}

pub struct TenantMiddleware;

impl Middleware for TenantMiddleware {
    fn handle<'a>(
        &'a self,
        mut req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let id = req
                .headers()
                .get("x-tenant-id")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("default")
                .to_string();

            req.extensions_mut().insert(TenantId(id));
            next.run(req).await
        })
    }
}
```

The handler receives `TenantId` like any other extractor:

```rust
#[get("/data")]
async fn handler(tenant: TenantId) -> Json<serde_json::Value> {
    Json(serde_json::json!({ "tenant": tenant.0 }))
}
```

### Modifying the response

Run `next.run(req).await` first, then mutate the response before returning it:

```rust
use rapina::middleware::BoxFuture;
use rapina::response::BoxBody;
use hyper::{Request, Response, body::Incoming};

pub struct SecurityHeadersMiddleware;

impl Middleware for SecurityHeadersMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let mut res = next.run(req).await;
            let h = res.headers_mut();
            h.insert("X-Content-Type-Options", "nosniff".parse().unwrap());
            h.insert("X-Frame-Options", "DENY".parse().unwrap());
            h.insert("Strict-Transport-Security", "max-age=31536000".parse().unwrap());
            res
        })
    }
}
```

---

## Tower Compatibility

Rapina can interop with the [Tower](https://docs.rs/tower) ecosystem via the `tower` feature flag. This lets you use battle-tested Tower layers (retry, circuit breakers, concurrency limits, etc.) as Rapina middleware.

```toml
rapina = { version = "0.11.0", features = ["tower"] }
```

### Using Tower layers as middleware

Use `.layer()` to add any Tower `Layer` to the middleware stack:

```rust
use rapina::prelude::*;
use tower::timeout::TimeoutLayer;
use std::time::Duration;

Rapina::new()
    .layer(TimeoutLayer::new(Duration::from_secs(30)))
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

Or wrap it explicitly with `TowerLayerMiddleware`:

```rust
use rapina::middleware::TowerLayerMiddleware;
use tower::timeout::TimeoutLayer;
use std::time::Duration;

Rapina::new()
    .middleware(TowerLayerMiddleware::new(TimeoutLayer::new(Duration::from_secs(30))))
    .discover()
    .listen("127.0.0.1:3000")
    .await
```

Tower layers that preserve the response body type (`Response<BoxBody>`) work directly. Errors from Tower services are logged and converted to `500 Internal Server Error` responses.

### Rapina as a Tower Service

`RapinaService` wraps Rapina's middleware + router stack as a `tower::Service`, useful for embedding Rapina in Tower-based infrastructure:

```rust
use rapina::middleware::RapinaService;

let service = RapinaService::new(router, state, middlewares);
// `service` implements tower::Service<Request<Incoming>>
```

---

## Middleware ordering

Middleware executes in **FIFO order** — first registered, first to run on the request and last to run on the response.

```
Request  →  [A]  →  [B]  →  [C]  →  Handler
Response ←  [A]  ←  [B]  ←  [C]  ←  Handler
```

> **Note on authentication:** `.with_auth()` is always appended last during `listen()`, after all middleware registered via `.middleware()`, `.with_cors()`, `.with_rate_limit()`, and `.with_compression()`.

### Recommended order

| Middleware | Position | Reason |
|------------|----------|--------|
| Trace ID | First | All downstream logs carry the request ID |
| Request Log | After Trace ID | Captures the trace ID in the log span |
| CORS | Before rate limit | Preflights are answered before consuming any quota |
| Rate limit | Before auth | No JWT work done for clients that will be blocked |
| Compression | After rate limit | Compresses the final response including error bodies |
| Auth | Last (framework-managed) | Always appended after user-registered middleware |

```rust
use rapina::middleware::{CorsConfig, CompressionConfig, TraceIdMiddleware, RequestLogConfig};

Rapina::new()
    .middleware(TraceIdMiddleware::new())
    .with_request_log(RequestLogConfig::verbose())
    .with_cors(CorsConfig::with_origins(vec!["https://app.example.com".to_string()]))
    .with_rate_limit(RateLimitConfig::per_minute(60))
    .with_compression(CompressionConfig::default())
    .middleware(SecurityHeadersMiddleware)
    .discover()
    .listen("127.0.0.1:3000")
    .await
```
