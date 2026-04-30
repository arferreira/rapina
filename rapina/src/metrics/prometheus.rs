use std::sync::Arc;

use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use prometheus::{
    CounterVec, Encoder, HistogramOpts, HistogramVec, IntGauge, Opts, Registry, TextEncoder,
};

use crate::extract::PathParams;
use crate::response::{BoxBody, PROMETHEUS_TEXT_FORMAT};
use crate::state::AppState;
use http::header::CONTENT_TYPE;

#[derive(Clone)]
pub struct MetricsRegistry {
    pub(crate) registry: Arc<Registry>,
    pub(crate) http_requests_total: CounterVec,
    pub(crate) http_request_duration_seconds: HistogramVec,
    pub(crate) http_requests_in_flight: IntGauge,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        let registry = Registry::new();

        let http_requests_total = CounterVec::new(
            Opts::new("http_requests_total", "Total number of HTTP requests"),
            &["method", "path", "status"],
        )
        .expect("failed to create http_requests_total metric");

        registry
            .register(Box::new(http_requests_total.clone()))
            .expect("failed to register http_requests_total");

        let http_request_duration_seconds = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds",
            ),
            &["method", "path"],
        )
        .expect("failed to create http_request_duration_seconds metric");

        registry
            .register(Box::new(http_request_duration_seconds.clone()))
            .expect("failed to register http_request_duration_seconds");

        let http_requests_in_flight = IntGauge::new(
            "http_requests_in_flight",
            "Number of HTTP requests currently being processed",
        )
        .expect("failed to create http_requests_in_flight metric");

        registry
            .register(Box::new(http_requests_in_flight.clone()))
            .expect("failed to register http_requests_in_flight");

        Self {
            registry: Arc::new(registry),
            http_requests_total,
            http_request_duration_seconds,
            http_requests_in_flight,
        }
    }

    /// Encodes all metrics in the Prometheus text exposition format.
    pub fn encode(&self) -> String {
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        let mut buffer = Vec::new();
        encoder
            .encode(&metric_families, &mut buffer)
            .unwrap_or_default();
        String::from_utf8(buffer).unwrap_or_default()
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Handler for the `GET /metrics` endpoint.
///
/// Returns all collected metrics in Prometheus text format.
pub async fn metrics_handler(
    _req: Request<Incoming>,
    _params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    match state.get::<MetricsRegistry>() {
        Some(registry) => {
            let body = registry.encode();
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, PROMETHEUS_TEXT_FORMAT)
                .body(crate::response::full(body))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(crate::response::empty())
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_registry_new() {
        let _registry = MetricsRegistry::new();
    }

    #[test]
    fn test_metrics_registry_default() {
        let _registry = MetricsRegistry::default();
    }

    #[test]
    fn test_metrics_registry_encode_empty_contains_metric_names() {
        let registry = MetricsRegistry::new();
        let output = registry.encode();

        // It only loads `http_requests_in_flight` because its from IntGauge type
        assert!(output.contains("http_requests_in_flight"));
    }

    #[test]
    fn test_metrics_registry_encode_prometheus_format() {
        let registry = MetricsRegistry::new();
        let output = registry.encode();
        assert!(output.contains("# HELP"));
        assert!(output.contains("# TYPE"));
    }

    #[test]
    fn test_metrics_registry_counter_increments() {
        let registry = MetricsRegistry::new();
        registry
            .http_requests_total
            .with_label_values(&["GET", "/health", "200"])
            .inc();

        let output = registry.encode();
        assert!(output.contains("http_requests_total"));
        assert!(output.contains(r#"method="GET""#));
        assert!(output.contains(r#"path="/health""#));
        assert!(output.contains(r#"status="200""#));
        assert!(output.contains("} 1"));
    }

    #[test]
    fn test_metrics_registry_in_flight_gauge() {
        let registry = MetricsRegistry::new();
        assert_eq!(registry.http_requests_in_flight.get(), 0);

        registry.http_requests_in_flight.inc();
        registry.http_requests_in_flight.inc();
        assert_eq!(registry.http_requests_in_flight.get(), 2);

        registry.http_requests_in_flight.dec();
        assert_eq!(registry.http_requests_in_flight.get(), 1);
    }

    #[test]
    fn test_metrics_registry_histogram_observe() {
        let registry = MetricsRegistry::new();
        registry
            .http_request_duration_seconds
            .with_label_values(&["POST", "/users"])
            .observe(0.042);

        let output = registry.encode();
        assert!(output.contains("http_request_duration_seconds"));
        assert!(output.contains(r#"method="POST""#));
    }

    #[test]
    fn test_metrics_registry_clone_shares_state() {
        let registry = MetricsRegistry::new();
        let clone = registry.clone();

        registry
            .http_requests_total
            .with_label_values(&["POST", "/", "200"])
            .inc();

        // The clone wraps the same Arc<Registry>, so its encode reflects the increment
        let output = clone.encode();
        assert!(output.contains("} 1"));
    }
}
