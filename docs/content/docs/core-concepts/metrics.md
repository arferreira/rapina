+++
title = "Metrics"
description = "Prometheus metrics with the metrics feature flag"
weight = 6
date = 2025-02-18
+++

Rapina can expose a `/metrics` endpoint in [Prometheus](https://prometheus.io/) text format. Enable it with the `metrics` feature flag.

## Setup

Add the feature to your `Cargo.toml`:

```toml
[dependencies]
rapina = { version = "0.12.0", features = ["metrics"] }
```

Enable the endpoint in your application:

```rust
use rapina::prelude::*;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .enable_metrics()
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
```

That's all. A `GET /metrics` route is registered automatically and returns the collected metrics in Prometheus text format.

## Dynamic configuration

When the value comes from a config struct or environment variable, use `with_metrics(bool)` to keep the builder chain intact:

```rust
let cfg = Config::from_env();

Rapina::new()
    .with_metrics(cfg.metrics_enabled)
    .router(router)
    .listen("127.0.0.1:3000")
    .await
```

Both forms are equivalent, `enable_metrics()` and `disable_metrics()` are convenience wrappers around `with_metrics(true/false)`.

## Collected Metrics

| Metric                          | Type      | Labels                     | Description                             |
| ------------------------------- | --------- | -------------------------- | --------------------------------------- |
| `http_requests_total`           | Counter   | `method`, `path`, `status` | Total number of HTTP requests completed |
| `http_request_duration_seconds` | Histogram | `method`, `path`           | Request duration in seconds             |
| `http_requests_in_flight`       | Gauge     | —                          | Requests currently being processed      |

Example output:

```
# HELP http_requests_total Total number of HTTP requests
# TYPE http_requests_total counter
http_requests_total{method="GET",path="/users",status="200"} 42
http_requests_total{method="POST",path="/users",status="201"} 7
http_requests_total{method="GET",path="/users/:id",status="404"} 3

# HELP http_request_duration_seconds HTTP request duration in seconds
# TYPE http_request_duration_seconds histogram
http_request_duration_seconds_bucket{method="GET",path="/users",le="0.005"} 38
http_request_duration_seconds_sum{method="GET",path="/users"} 0.312
http_request_duration_seconds_count{method="GET",path="/users"} 42

# HELP http_requests_in_flight Number of HTTP requests currently being processed
# TYPE http_requests_in_flight gauge
http_requests_in_flight 2
```

## Path Normalisation

The `path` label is the route pattern that matched, not the raw URL. Cardinality stays
bounded by the number of routes you registered, so a path param never inflates the label
set, whether it's a number, a UUID, or any other string:

| Route definition | Raw request path                              | Label value  |
| ---------------- | --------------------------------------------- | ------------ |
| `/users/:id`     | `/users/42`                                   | `/users/:id` |
| `/users/:id`     | `/users/e58ed763-928c-4155-bee9-fdbaaadc15f6` | `/users/:id` |
| `/users/:id`     | `/users/whatever`                             | `/users/:id` |

The label keeps the parameter name from the route definition, so `/orders/:order_id` shows
up as `/orders/:order_id` and matches what `rapina routes` prints.

Requests that match no route (404s) are labelled `<unmatched>`. They share a single time
series, so a client probing random URLs can't explode the metric. The full request path is
still recorded on the OpenTelemetry span (`url.path`) when tracing is enabled, which is where
high-cardinality data belongs.

## Custom Metrics

Register your own Prometheus collectors alongside the built-in HTTP metrics using `add_metric()`:

```rust
use rapina::prelude::*;
use rapina::prometheus::{IntCounterVec, Opts};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let orders_total = IntCounterVec::new(
        Opts::new("orders_total", "Total number of orders placed"),
        &["status"],
    )
    .unwrap();

    // Clone before passing so you can increment it from your handlers.
    let orders_counter = orders_total.clone();

    Rapina::new()
        .enable_metrics()
        .add_metric(orders_total)
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
```

All types that implement `prometheus::core::Collector` are accepted — `IntCounter`, `IntCounterVec`, `Gauge`, `Histogram`, `HistogramVec`, and any custom collector.

> **Name collisions:** Rapina panics at startup if a custom metric name clashes with a built-in metric (`http_requests_total`, `http_request_duration_seconds`, `http_requests_in_flight`) or with another previously registered custom collector. Use unique names to avoid this.

## Scraping with Prometheus

Point Prometheus at the `/metrics` endpoint in your `prometheus.yml`:

```yaml
scrape_configs:
  - job_name: my-rapina-api
    static_configs:
      - targets: ["localhost:3000"]
```
