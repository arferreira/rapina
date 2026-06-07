+++
title = "Observability"
description = "Structured logging and OpenTelemetry trace export with the otel feature flag"
weight = 13
date = 2026-06-07
+++

Rapina has two observability layers: structured logging built on [tracing](https://docs.rs/tracing), and distributed tracing exported over OTLP to any OpenTelemetry collector such as Jaeger, Grafana Tempo, or Datadog.

## Structured logging

`with_tracing` installs a global `tracing` subscriber that formats logs to stdout:

```rust
use rapina::prelude::*;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .with_tracing(TracingConfig::default())
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
```

`TracingConfig` controls the output format:

| Field              | Default       | Description                          |
| ------------------ | ------------- | ------------------------------------ |
| `json`             | `false`       | JSON output instead of plain text    |
| `level`            | `Level::INFO` | Minimum level to log                 |
| `with_target`      | `true`        | Include the module path              |
| `with_file`        | `false`       | Include the source file name         |
| `with_line_number` | `false`       | Include the source line number       |

Each field has a builder method:

```rust
TracingConfig::new()
    .json()
    .level(Level::DEBUG)
    .with_file(true)
    .with_line_number(true)
```

When the `RUST_LOG` environment variable is set, it takes precedence over `level` and supports the full [EnvFilter](https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html) syntax (`RUST_LOG=info,my_api=debug`).

## OpenTelemetry trace export

Enable the `otel` feature flag:

```toml
[dependencies]
rapina = { version = "0.12.0", features = ["otel"] }
```

Then configure the exporter with `with_telemetry`:

```rust
use rapina::prelude::*;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .with_telemetry(TelemetryConfig {
            endpoint: "http://localhost:4317".into(),
            service_name: "my-api".into(),
            sample_rate: 1.0,
        })
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
```

`TelemetryConfig` fields and defaults:

| Field          | Default                  | Description                                            |
| -------------- | ------------------------ | ------------------------------------------------------ |
| `endpoint`     | `http://localhost:4317`  | OTLP gRPC endpoint of the collector                    |
| `service_name` | `rapina`                 | Reported as the `service.name` resource attribute      |
| `sample_rate`  | `1.0`                    | Fraction of traces to sample, `0.0` (none) to `1.0` (all) |

The same builder style works here:

```rust
TelemetryConfig::new()
    .endpoint("http://jaeger:4317")
    .service_name("my-api")
    .sample_rate(0.25)
```

Export is plaintext OTLP gRPC by default. For an `https://` collector enable the `otel-tls` feature, which adds TLS with the system root certificates.

`with_telemetry` and `with_tracing` compose: when both are set, logs go to stdout and spans go to the collector through a single subscriber. Spans are exported in batches and flushed during graceful shutdown, so in-flight traces are not dropped.

## Sampling

The sampler is parent-based. When a request carries a propagated trace context, the upstream sampling decision is honored; otherwise the trace id is sampled at `sample_rate`. Out-of-range values are clamped to `[0.0, 1.0]`.

## Request spans

With telemetry configured, every request gets a server span following the [OpenTelemetry HTTP semantic conventions](https://opentelemetry.io/docs/specs/semconv/http/http-spans/). The span is named `{method} {route}` using the matched route template (`GET /users/:id`, not `GET /users/42`), falling back to the bare method when no route matches.

Recorded attributes:

| Attribute                   | Example       |
| --------------------------- | ------------- |
| `http.request.method`       | `GET`         |
| `http.route`                | `/users/:id`  |
| `url.path`                  | `/users/42`   |
| `http.response.status_code` | `200`         |

Responses with a 5xx status mark the span as an error. Log lines emitted inside a request carry `otel_trace_id` and `otel_span_id` fields, so logs correlate with the exported trace.

## Distributed tracing

Incoming requests with a W3C `traceparent` header continue the upstream trace: the request span is linked to the remote parent, and spans exported from your service appear in the same trace as the calling service.

## Trying it locally

Run Jaeger all-in-one:

```sh
docker run --rm -p 4317:4317 -p 16686:16686 jaegertracing/all-in-one
```

Start your app with the `otel` feature and default `TelemetryConfig`, send a few requests, and open the Jaeger UI at `http://localhost:16686`. The repository ships a runnable example: `cargo run --example telemetry --features otel`.
