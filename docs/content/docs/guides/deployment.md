+++
title = "Deployment"
description = "Deploying a Rapina application to production"
weight = 1
date = 2026-03-15
+++

Rapina compiles to a single static binary with no runtime dependencies, making deployment straightforward on any platform that runs Linux, macOS, or Windows.

## Building for Release

Build your application with optimizations enabled:

```bash
cargo build --release
```

The binary is at `target/release/<your-app-name>`. This single file is all you need to deploy — no runtime, no interpreter, no dependency folder.

If your app uses a database, make sure the correct feature flag is enabled in your `Cargo.toml`:

```toml
[dependencies]
rapina = { version = "0.13.1", features = ["postgres"] }
```

Available database features: `postgres`, `mysql`, `sqlite`.

---

## Environment Variables and Configuration

Rapina reads all configuration from environment variables. In production, set these directly in your hosting platform rather than relying on a `.env` file.

### Application config

Use the `#[derive(Config)]` macro with `#[env]` and `#[default]` attributes:

```rust
#[derive(Config)]
struct AppConfig {
    #[env = "HOST"]
    #[default = "0.0.0.0"]
    host: String,

    #[env = "PORT"]
    #[default = "3000"]
    port: u16,
}
```

> **Note:** Bind to `0.0.0.0` in production (not `127.0.0.1`) so the server is reachable from outside the container or host.

### Database config

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `DATABASE_URL` | Yes | — | Connection string (e.g. `postgres://user:pass@host/db`) |
| `DATABASE_MAX_CONNECTIONS` | No | `10` | Connection pool maximum |
| `DATABASE_MIN_CONNECTIONS` | No | `1` | Connection pool minimum |
| `DATABASE_CONNECT_TIMEOUT` | No | `30` | Seconds before connection attempt fails |
| `DATABASE_IDLE_TIMEOUT` | No | `600` | Seconds before idle connections are closed |
| `DATABASE_LOGGING` | No | `false` in release | Log SQL queries |

### Auth config

| Variable | Required | Default | Description |
|----------|----------|---------|-------------|
| `JWT_SECRET` | Yes | — | Secret key for signing JWTs |
| `JWT_EXPIRATION` | No | `3600` | Token lifetime in seconds |

### Logging

Set the `RUST_LOG` environment variable to control log verbosity. This takes precedence over the programmatic `.level()` setting on `TracingConfig`.

```bash
RUST_LOG=info          # recommended for production
RUST_LOG=warn          # quieter — only warnings and errors
RUST_LOG=myapp=debug   # debug logs for your crate, info for everything else
```

---

## Health Check Endpoint

Rapina ships a built-in health check at `GET /__rapina/health`. Enable it with:

```rust
Rapina::new()
    .with_health_check(true)
```

The endpoint returns `200 OK` and is automatically public (no authentication required). Point your load balancer or orchestrator at `/__rapina/health`.

---

## Startup Hooks

`on_startup` registers async hooks that run once, at boot, before the server accepts any traffic. Use it for warm-up work that must succeed before the application is considered ready: preloading a cache, validating configuration, or failing fast when an upstream dependency is unreachable.

### Register hooks

```rust
use rapina::prelude::*;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .on_startup(|| async {
            tracing::info!("Preloading feature flags...");
            Ok(())
        })
        .on_startup(|| async {
            tracing::info!("Validating configuration...");
            Ok(())
        })
        .listen("0.0.0.0:3000")
        .await
}
```

Hooks return `Result<(), Box<dyn std::error::Error + Send + Sync>>`, so any error that converts into that box works, either through `?` or with `.into()`: `std::io::Error`, a `thiserror` enum, a database error, or an already-boxed trait object. Both the hook and the future it returns must be `Send + 'static`.

A hook only gets what it captures, so it cannot reach the app state through `Rapina`. Register the resource before `.on_startup()` and clone a handle into the closure, exactly as described in [Graceful Shutdown](@/docs/core-concepts/state.md#graceful-shutdown):

```rust
let pool = MyPool::connect().await?;

Rapina::new()
    .state(pool.clone())
    .on_startup(move || {
        let pool = pool.clone();
        async move {
            pool.warm_up().await?;
            Ok(())
        }
    })
    .listen("0.0.0.0:3000")
    .await
```

### Execution order and timing

Hooks run sequentially in the order they were registered, and exactly once per `listen()` call.

1. Deferred setup is applied (`prepare()`): discovered routes, middleware, auth
2. Startup hooks run, in registration order
3. The TCP listener binds
4. Requests are accepted

Both steps 1 and 2 happen before the socket exists, so if a hook fails, no request ever reaches your application. This is the difference between a failed deploy and a process that comes up healthy and then returns `500` on the first real request.

### Failure behaviour

The first hook that returns `Err` aborts startup. Remaining hooks do not run, and `listen()` returns `std::io::Error` whose message identifies the hook by 1-based index, for example `startup hook 2 failed: connection refused`. The original error is kept inside that `io::Error`, reachable through `get_ref()` and `source()`, so callers can inspect or downcast it.

Propagate the error from `main` so the process exits non-zero and your orchestrator sees a failed boot:

```rust
#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .on_startup(|| async {
            // Any `Err` here aborts startup with `startup hook 1 failed: ...`.
            Err(std::io::Error::other("database unreachable").into())
        })
        .listen("0.0.0.0:3000")
        .await
}
```

### Panics

A hook that panics is caught, whether it panics before returning its future or while the future is being polled. Remaining hooks do not run, and `listen()` returns `std::io::Error` with a message like `startup hook 1 panicked: ...`, carrying the panic payload (non-string payloads are reported as `non-string panic payload`). A panic in a startup hook therefore turns into a failed boot rather than an unwinding server. The default panic hook still prints the panic message to stderr; `RUST_BACKTRACE=1` adds a backtrace. Only the unwind is contained, not the log line.

> **Note:** catching a panic relies on unwinding. If you build with `panic = "abort"` (a profile setting, e.g. `[profile.release] panic = "abort"`), a panicking hook aborts the process as any other panic would; startup hooks are not special-cased. Keep the default unwind strategy if you want panics converted into a `listen()` error.

### Testing startup hooks

`TestClient` does not go through `listen()`/`serve()`, so it skips both `on_startup` and `on_shutdown` hooks, which keeps request tests fast and side-effect free. To test the hooks themselves, call `listen()` and assert on its result.

---

## Graceful Shutdown

Rapina handles `SIGINT` (Ctrl-C) and `SIGTERM` automatically. When a signal is received:

1. The server stops accepting new connections
2. In-flight requests are given time to complete (default: 30 seconds)
3. Shutdown hooks run sequentially
4. The process exits

### Configure the drain timeout

```rust
use std::time::Duration;

Rapina::new()
    .shutdown_timeout(Duration::from_secs(60))
```

### Run cleanup on shutdown

```rust
Rapina::new()
    .on_shutdown(|| async {
        tracing::info!("Flushing metrics...");
        // cleanup logic here
    })
```

Hooks execute after connections drain (or after the timeout expires), in the order they were registered.

---

## Docker Setup

Since there are no runtime dependencies, a multi-stage build produces a minimal image.

### Dockerfile

```dockerfile
# Build stage
FROM rust:1.85-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# If you have migrations:
# COPY migrations ./migrations

RUN cargo build --release

# Runtime stage
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/myapp /usr/local/bin/myapp

EXPOSE 3000
CMD ["myapp"]
```

### .dockerignore

```
target/
.git/
.env
*.db
```

### Build and run

```bash
docker build -t myapp .
docker run -p 3000:3000 \
  -e DATABASE_URL=postgres://user:pass@host/db \
  -e JWT_SECRET=your-production-secret \
  -e RUST_LOG=info \
  myapp
```

> **Tip:** For even smaller images, use `FROM scratch` or `FROM gcr.io/distroless/cc-debian12` as the runtime stage if your app doesn't need a shell or package manager. You may need to statically link with `RUSTFLAGS='-C target-feature=+crt-static'` and target `x86_64-unknown-linux-gnu`.

---

## Running Behind a Reverse Proxy

In production, place your Rapina app behind a reverse proxy for TLS termination, static file serving, and load balancing.

### Nginx

```nginx
upstream rapina {
    server 127.0.0.1:3000;
}

server {
    listen 443 ssl;
    server_name api.example.com;

    ssl_certificate     /etc/ssl/certs/api.example.com.pem;
    ssl_certificate_key /etc/ssl/private/api.example.com.key;

    location / {
        proxy_pass http://rapina;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

### Caddy

```
api.example.com {
    reverse_proxy 127.0.0.1:3000
}
```

Caddy handles TLS certificates automatically via Let's Encrypt.

### Important: IP extraction behind a proxy

Rapina's rate limiter reads client IPs from `X-Forwarded-For` (leftmost entry), then `X-Real-IP`, falling back to `"unknown"`. Make sure your reverse proxy sets these headers so rate limiting and logging reflect the real client IP.

---

## Deployment Targets

### Railway

Railway detects Rust projects automatically via `Cargo.toml`.

1. Push your code to a GitHub repository
2. Create a new project on Railway and connect the repo
3. Set environment variables in the Railway dashboard
4. Bind to `0.0.0.0` and use the `PORT` environment variable Railway provides:

```rust
#[derive(Config)]
struct AppConfig {
    #[env = "HOST"]
    #[default = "0.0.0.0"]
    host: String,

    #[env = "PORT"]
    #[default = "3000"]
    port: u16,
}
```

### Fly.io

Create a `fly.toml`:

```toml
app = "myapp"
primary_region = "iad"

[build]

[http_service]
  internal_port = 3000
  force_https = true

[checks]
  [checks.health]
    port = 3000
    type = "http"
    interval = "10s"
    timeout = "2s"
    path = "/__rapina/health"
```

Deploy with the Fly CLI:

```bash
fly launch
fly secrets set DATABASE_URL=postgres://...
fly secrets set JWT_SECRET=your-production-secret
fly deploy
```

### AWS ECS

1. Build and push your Docker image to ECR
2. Create a task definition referencing the image
3. Pass environment variables via the task definition or AWS Secrets Manager
4. Configure an ALB target group with a health check on `/__rapina/health`
5. Set the ECS service desired count for availability

### Bare Metal / VPS

Copy the release binary to your server, set environment variables, and run it behind a reverse proxy:

```bash
scp target/release/myapp user@server:/opt/myapp/
```

Create a systemd service:

```ini
[Unit]
Description=My Rapina App
After=network.target

[Service]
Type=simple
ExecStart=/opt/myapp/myapp
Environment=DATABASE_URL=postgres://localhost/myapp
Environment=JWT_SECRET=your-production-secret
Environment=RUST_LOG=info
Restart=on-failure
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl enable myapp
sudo systemctl start myapp
```

Rapina handles `SIGTERM` from systemd gracefully, draining connections before exiting.

---

## Production Checklist

### Logging and tracing

```rust
Rapina::new()
    .with_tracing(TracingConfig::new().json().level(tracing::Level::INFO))
    .middleware(RequestLogMiddleware::new())
```

- Use `.json()` for structured logs that integrate with log aggregators (Datadog, Loki, CloudWatch)
- Add `RequestLogMiddleware` to log method, path, status, and duration for every request
- Set `RUST_LOG=info` in production

### Metrics

Enable Prometheus metrics for monitoring:

```toml
[dependencies]
rapina = { version = "0.13.1", features = ["metrics"] }
```

```rust
Rapina::new()
    .with_metrics(true)
```

This exposes `GET /metrics` with `http_requests_total`, `http_request_duration_seconds`, and `http_requests_in_flight`. Point Prometheus or your monitoring stack at this endpoint.

### Rate limiting

```rust
Rapina::new()
    .with_rate_limit(RateLimitConfig::per_minute(60))
```

### CORS

Lock down origins in production:

```rust
Rapina::new()
    .with_cors(CorsConfig::with_origins(vec![
        "https://app.example.com".to_string(),
    ]))
```

### Request safeguards

```rust
use rapina::middleware::{TimeoutMiddleware, BodyLimitMiddleware, TraceIdMiddleware};
use std::time::Duration;

Rapina::new()
    .middleware(TraceIdMiddleware::new())
    .middleware(TimeoutMiddleware::new(Duration::from_secs(30)))
    .middleware(BodyLimitMiddleware::new(2 * 1024 * 1024)) // 2 MB
```

### Full production example

```rust
use rapina::prelude::*;
use rapina::middleware::{
    CorsConfig, CompressionConfig, TraceIdMiddleware,
    TimeoutMiddleware, BodyLimitMiddleware, RequestLogMiddleware,
};
use std::time::Duration;

#[derive(Config)]
struct AppConfig {
    #[env = "HOST"]
    #[default = "0.0.0.0"]
    host: String,

    #[env = "PORT"]
    #[default = "3000"]
    port: u16,
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    load_dotenv();

    let config = AppConfig::from_env().expect("Missing config");
    let addr = format!("{}:{}", config.host, config.port);

    Rapina::new()
        .with_health_check(true)
        .with_tracing(TracingConfig::new().json().level(tracing::Level::INFO))
        .middleware(TraceIdMiddleware::new())
        .middleware(RequestLogMiddleware::new())
        .middleware(TimeoutMiddleware::new(Duration::from_secs(30)))
        .middleware(BodyLimitMiddleware::new(2 * 1024 * 1024))
        .with_cors(CorsConfig::with_origins(vec![
            "https://app.example.com".to_string(),
        ]))
        .with_rate_limit(RateLimitConfig::per_minute(60))
        .with_compression(CompressionConfig::default())
        .with_metrics(true)
        .shutdown_timeout(Duration::from_secs(60))
        .on_shutdown(|| async {
            tracing::info!("Application shutting down");
        })
        .state(config)
        .discover()
        .listen(&addr)
        .await
}
```
