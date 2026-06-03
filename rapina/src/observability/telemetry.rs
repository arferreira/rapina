use opentelemetry::trace::TracerProvider as _;
use opentelemetry_otlp::{ExporterBuildError, SpanExporter, WithExportConfig};
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
use tracing_subscriber::{Layer, Registry};

/// Configuration for exporting traces over OTLP to a collector such as Jaeger
/// or Datadog.
///
/// Export is plaintext OTLP gRPC by default. For an `https://` collector enable
/// the `otel-tls` feature, which adds TLS with the system root certificates.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// Rapina::new()
///     .with_telemetry(TelemetryConfig {
///         endpoint: "http://jaeger:4317".into(),
///         service_name: "my-api".into(),
///         sample_rate: 1.0,
///     })
///     .router(router)
///     .listen("127.0.0.1:3000")
///     .await
/// ```
#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// OTLP gRPC endpoint of the collector (for example `http://jaeger:4317`).
    pub endpoint: String,
    /// Service name reported as the `service.name` resource attribute.
    pub service_name: String,
    /// Fraction of traces to sample, from `0.0` (none) to `1.0` (all).
    pub sample_rate: f64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4317".to_string(),
            service_name: "rapina".to_string(),
            sample_rate: 1.0,
        }
    }
}

impl TelemetryConfig {
    /// Creates a telemetry configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the OTLP collector endpoint.
    pub fn endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.endpoint = endpoint.into();
        self
    }

    /// Sets the service name.
    pub fn service_name(mut self, name: impl Into<String>) -> Self {
        self.service_name = name.into();
        self
    }

    /// Sets the sampling rate (`0.0` to `1.0`).
    pub fn sample_rate(mut self, rate: f64) -> Self {
        self.sample_rate = rate;
        self
    }
}

/// Clamps a sampling rate into `[0.0, 1.0]`, mapping NaN to full sampling so a
/// misconfigured value never silently disables tracing.
fn normalize_sample_rate(rate: f64) -> f64 {
    if rate.is_nan() {
        1.0
    } else {
        rate.clamp(0.0, 1.0)
    }
}

/// Builds the OTLP tracing pipeline.
///
/// Returns the tracer provider (kept so it can be installed globally and flushed
/// on shutdown) and the `tracing-opentelemetry` layer to compose onto the
/// subscriber. The sampler is parent-based so it honors the sampling decision
/// carried in a propagated trace context. Installing the provider globally is
/// left to the caller so all process-global mutation stays in one place.
pub(crate) fn build_otel_pipeline(
    config: &TelemetryConfig,
) -> Result<(SdkTracerProvider, Box<dyn Layer<Registry> + Send + Sync>), ExporterBuildError> {
    let exporter = SpanExporter::builder()
        .with_tonic()
        .with_endpoint(&config.endpoint)
        .build()?;

    let resource = Resource::builder()
        .with_service_name(config.service_name.clone())
        .build();

    let provider = SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_sampler(Sampler::ParentBased(Box::new(Sampler::TraceIdRatioBased(
            normalize_sample_rate(config.sample_rate),
        ))))
        .with_resource(resource)
        .build();

    let tracer = provider.tracer("rapina");
    let layer = tracing_opentelemetry::layer().with_tracer(tracer).boxed();

    Ok((provider, layer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_values() {
        let config = TelemetryConfig::default();
        assert_eq!(config.endpoint, "http://localhost:4317");
        assert_eq!(config.service_name, "rapina");
        assert_eq!(config.sample_rate, 1.0);
    }

    #[test]
    fn builder_overrides_fields() {
        let config = TelemetryConfig::new()
            .endpoint("http://jaeger:4317")
            .service_name("my-api")
            .sample_rate(0.25);
        assert_eq!(config.endpoint, "http://jaeger:4317");
        assert_eq!(config.service_name, "my-api");
        assert_eq!(config.sample_rate, 0.25);
    }

    #[test]
    fn sample_rate_is_normalized() {
        assert_eq!(normalize_sample_rate(0.5), 0.5);
        assert_eq!(normalize_sample_rate(-1.0), 0.0);
        assert_eq!(normalize_sample_rate(2.0), 1.0);
        assert_eq!(normalize_sample_rate(f64::NAN), 1.0);
    }
}
