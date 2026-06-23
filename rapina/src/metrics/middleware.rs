use std::time::Instant;

use crate::context::{MatchedPattern, RequestContext};
use crate::middleware::{BoxFuture, Middleware, Next};
use crate::response::BoxBody;
use hyper::body::Incoming;
use hyper::{Request, Response};
use prometheus::IntGauge;

use super::prometheus::MetricsRegistry;

pub struct MetricsMiddleware {
    registry: MetricsRegistry,
}

impl MetricsMiddleware {
    pub fn new(registry: MetricsRegistry) -> Self {
        Self { registry }
    }
}

/// RAII guard that safely manages the `http_requests_in_flight` metric.
///
/// In Hyper/Tokio, if a client closes the TCP connection mid-request,
/// the executing Future is abruptly dropped. If we manually called `.inc()`
/// and `.dec()` in the middleware, the `.dec()` would never execute upon
/// cancellation, causing a permanent metric leak.
///
/// This guard ensures that the gauge is strictly incremented on creation
/// and guaranteed to be decremented when dropped, regardless of whether
/// the request succeeds or is cancelled by the runtime.
struct InFlightGuard {
    gauge: IntGauge,
}

impl InFlightGuard {
    /// Constructs a new Inflight Guard, which automatically increments the Prometheus gauge
    fn new(gauge: IntGauge) -> Self {
        gauge.inc();
        Self { gauge }
    }
}

impl Drop for InFlightGuard {
    /// Drops the Inflight Guard, which automatically decrements the Prometheus gauge
    fn drop(&mut self) {
        self.gauge.dec();
    }
}

/// Shared label for unmatched routes, so 404 traffic can't mint a fresh time
/// series per distinct URL.
const UNMATCHED: &str = "<unmatched>";

impl Middleware for MetricsMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        let method = req.method().to_string();
        let path = req
            .extensions()
            .get::<MatchedPattern>()
            .map(|m| m.as_str().to_owned())
            .unwrap_or_else(|| UNMATCHED.to_owned());
        let registry = self.registry.clone();

        Box::pin(async move {
            let _in_flight_guard = InFlightGuard::new(registry.http_requests_in_flight.clone());

            let start = Instant::now();
            let response = next.run(req).await;
            let duration = start.elapsed().as_secs_f64();

            let status = response.status().as_u16().to_string();
            registry
                .http_requests_total
                .with_label_values(&[&method, &path, &status])
                .inc();
            registry
                .http_request_duration_seconds
                .with_label_values(&[&method, &path])
                .observe(duration);

            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_middleware_new() {
        let registry = MetricsRegistry::new();
        let _middleware = MetricsMiddleware::new(registry);
    }

    #[test]
    fn test_in_flight_guard_increments_and_decrements_cleanly() {
        let registry = MetricsRegistry::new();
        let gauge = registry.http_requests_in_flight.clone();

        assert_eq!(gauge.get(), 0, "In-Flight Gauge should start at 0");

        {
            let _guard = InFlightGuard::new(gauge.clone());
            assert_eq!(
                gauge.get(),
                1,
                "In-Flight Gauge should be 1 while guard is in scope"
            );
        }

        assert_eq!(
            gauge.get(),
            0,
            "In-Flight Gauge should return to 0 after guard drops"
        );
    }
}
