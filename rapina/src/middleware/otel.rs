use hyper::body::Incoming;
use hyper::{Request, Response};
use opentelemetry::global;
use opentelemetry::propagation::Extractor;
use opentelemetry::trace::TraceContextExt;
use tracing::field::Empty;
use tracing::{Instrument, info_span};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::context::RequestContext;
use crate::response::BoxBody;

use super::{BoxFuture, Middleware, Next};

/// Adapts an HTTP header map to the OpenTelemetry propagation `Extractor` so a
/// W3C `traceparent` (and friends) can be read from an incoming request.
struct HeaderExtractor<'a>(&'a http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|name| name.as_str()).collect()
    }
}

/// Middleware that continues a distributed trace from incoming request headers.
///
/// It extracts the remote trace context (W3C `traceparent`) using the globally
/// configured propagator and attaches it as the parent of the request span, so
/// spans exported over OTLP link back to the calling service. Registered
/// automatically at the front of the stack when telemetry is configured.
#[derive(Debug, Clone, Copy)]
pub struct TraceContextMiddleware;

impl TraceContextMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TraceContextMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for TraceContextMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        let parent_cx = global::get_text_map_propagator(|propagator| {
            propagator.extract(&HeaderExtractor(req.headers()))
        });
        let span = server_span(req.method(), req.uri().path(), parent_cx);

        let response_span = span.clone();
        Box::pin(
            async move {
                let response = next.run(req).await;
                record_response(&response_span, response.status());
                response
            }
            .instrument(span),
        )
    }
}

/// Builds the server request span following the OpenTelemetry HTTP semantic
/// conventions, links it to the remote parent, and records the OTel ids onto it
/// so log lines emitted within the span correlate with the exported trace.
///
/// The route template isn't known before routing, so the span name is the
/// method per the spec's low-cardinality fallback.
fn server_span(
    method: &hyper::Method,
    path: &str,
    parent_cx: opentelemetry::Context,
) -> tracing::Span {
    let span = info_span!(
        "http.request",
        otel.name = %method,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %method,
        url.path = %path,
        http.response.status_code = Empty,
        otel_trace_id = Empty,
        otel_span_id = Empty,
    );
    let _ = span.set_parent(parent_cx);

    let span_context = span.context().span().span_context().clone();
    if span_context.is_valid() {
        span.record("otel_trace_id", span_context.trace_id().to_string());
        span.record("otel_span_id", span_context.span_id().to_string());
    }
    span
}

/// Records the response status onto the request span, marking server errors.
fn record_response(span: &tracing::Span, status: hyper::StatusCode) {
    span.record("http.response.status_code", status.as_u16());
    if status.is_server_error() {
        span.record("otel.status_code", "ERROR");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry::propagation::TextMapPropagator;
    use opentelemetry::trace::TraceContextExt;
    use opentelemetry_sdk::propagation::TraceContextPropagator;

    #[test]
    fn extracts_w3c_traceparent_into_remote_context() {
        let trace_id = "0af7651916cd43dd8448eb211c80319c";
        let parent_span_id = "b7ad6b7169203331";
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "traceparent",
            format!("00-{trace_id}-{parent_span_id}-01")
                .parse()
                .unwrap(),
        );

        let propagator = TraceContextPropagator::new();
        let cx = propagator.extract(&HeaderExtractor(&headers));
        let remote = cx.span().span_context().clone();

        assert!(remote.is_remote());
        assert_eq!(format!("{:032x}", remote.trace_id()), trace_id);
        assert_eq!(format!("{:016x}", remote.span_id()), parent_span_id);
    }

    #[test]
    fn missing_traceparent_yields_invalid_context() {
        let headers = http::HeaderMap::new();
        let propagator = TraceContextPropagator::new();
        let cx = propagator.extract(&HeaderExtractor(&headers));

        assert!(!cx.span().span_context().is_valid());
    }

    #[test]
    fn exported_span_continues_remote_trace_and_records_status() {
        use opentelemetry::trace::{Status, TracerProvider as _};
        use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
        use tracing_subscriber::Registry;
        use tracing_subscriber::layer::SubscriberExt;

        let trace_id = "0af7651916cd43dd8448eb211c80319c";
        let parent_span_id = "b7ad6b7169203331";
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "traceparent",
            format!("00-{trace_id}-{parent_span_id}-01")
                .parse()
                .unwrap(),
        );

        let exporter = InMemorySpanExporter::default();
        let provider = SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build();
        let subscriber = Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));

        // Drive the real span helpers under a scoped subscriber so we don't touch
        // the global one. Dropping the span closes it and the simple exporter
        // records it synchronously.
        tracing::subscriber::with_default(subscriber, || {
            let parent_cx = TraceContextPropagator::new().extract(&HeaderExtractor(&headers));
            let span = server_span(&hyper::Method::GET, "/users/42", parent_cx);
            record_response(&span, hyper::StatusCode::INTERNAL_SERVER_ERROR);
        });

        let spans = exporter.get_finished_spans().unwrap();
        assert_eq!(spans.len(), 1);
        let span = &spans[0];

        assert_eq!(format!("{:032x}", span.span_context.trace_id()), trace_id);
        assert_eq!(format!("{:016x}", span.parent_span_id), parent_span_id);
        assert_eq!(span.status, Status::error(""));

        let attr = |key: &str| {
            span.attributes
                .iter()
                .find(|kv| kv.key.as_str() == key)
                .map(|kv| kv.value.to_string())
        };
        assert_eq!(attr("http.request.method").as_deref(), Some("GET"));
        assert_eq!(attr("url.path").as_deref(), Some("/users/42"));
        assert_eq!(attr("http.response.status_code").as_deref(), Some("500"));
    }
}
