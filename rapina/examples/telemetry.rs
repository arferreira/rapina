//! Export traces over OTLP to a collector such as Jaeger or Datadog.
//!
//! Run a local collector, for example Jaeger all-in-one:
//!
//! ```sh
//! docker run --rm -p 4317:4317 -p 16686:16686 jaegertracing/all-in-one
//! ```
//!
//! Then `cargo run --example telemetry --features otel` and open the Jaeger UI
//! at http://localhost:16686. Requests carrying a W3C `traceparent` header
//! continue the upstream trace.

use rapina::prelude::*;

#[get("/")]
async fn hello() -> &'static str {
    "Hello, Rapina!"
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    Rapina::new()
        .with_telemetry(TelemetryConfig {
            endpoint: "http://localhost:4317".into(),
            service_name: "my-api".into(),
            sample_rate: 1.0,
        })
        .discover()
        .listen("127.0.0.1:3000")
        .await
}
