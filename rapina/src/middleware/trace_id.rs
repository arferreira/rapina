use hyper::body::Incoming;
use hyper::header::HeaderValue;
use hyper::{Request, Response};

use crate::context::RequestContext;
use crate::response::BoxBody;

use super::{BoxFuture, Middleware, Next};

pub const TRACE_ID_HEADER: &str = "x-trace-id";

const REQUEST_ID_HEADER: &str = "x-request-id";

const MAX_TRACE_ID_LEN: usize = 128;

/// Middleware that propagates a trace ID through the request/response cycle.
///
/// If the incoming request contains an `x-trace-id` header (or, when that is
/// absent, an `x-request-id` header) with a valid ID, its value is used as the
/// trace ID for the request context; otherwise the auto-generated UUID from
/// [`RequestContext`] is kept. A valid ID is at most 128 bytes and contains
/// only ASCII letters, digits, `-`, `_`, `.` and `:`. The final trace ID is
/// echoed back to the caller via an `x-trace-id` response header, enabling
/// distributed tracing across service boundaries.
///
/// [`RequestContext`]: crate::context::RequestContext
///
/// # Example
///
/// ```rust,ignore
/// Rapina::new()
///     .with(TraceIdMiddleware::new())
/// ```
#[derive(Debug, Clone, Copy)]
pub struct TraceIdMiddleware;

impl TraceIdMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TraceIdMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Middleware for TraceIdMiddleware {
    fn handle<'a>(
        &'a self,
        mut req: Request<Incoming>,
        ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let headers = req.headers();
            let incoming_trace_id = headers
                .get(TRACE_ID_HEADER)
                .or_else(|| headers.get(REQUEST_ID_HEADER))
                .and_then(|v| v.to_str().ok())
                .filter(|id| is_valid_trace_id(id))
                .map(String::from);

            let trace_id = if let Some(id) = incoming_trace_id {
                // Use the provided trace_id and update context in extensions
                let new_ctx = RequestContext::with_trace_id(id.clone());
                req.extensions_mut().insert(new_ctx);
                id
            } else {
                ctx.trace_id().to_owned()
            };

            let mut response = next.run(req).await;

            // Add x-trace-id to response headers
            if let Ok(header_value) = HeaderValue::from_str(&trace_id) {
                response.headers_mut().insert(TRACE_ID_HEADER, header_value);
            }

            response
        })
    }
}

fn is_valid_trace_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_TRACE_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_valid_trace_id_accepts_id_chars() {
        assert!(is_valid_trace_id("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_valid_trace_id("Req-1_abc.def:42"));
        assert!(is_valid_trace_id(&"a".repeat(MAX_TRACE_ID_LEN)));
    }

    #[test]
    fn test_is_valid_trace_id_rejects_empty() {
        assert!(!is_valid_trace_id(""));
    }

    #[test]
    fn test_is_valid_trace_id_rejects_too_long() {
        assert!(!is_valid_trace_id(&"a".repeat(MAX_TRACE_ID_LEN + 1)));
    }

    #[test]
    fn test_is_valid_trace_id_rejects_other_chars() {
        for id in ["a b", "a/b", "a=b", "a;b", "a\"b", "a\tb", "trace-é"] {
            assert!(!is_valid_trace_id(id), "{id:?} should be rejected");
        }
    }

    #[test]
    fn test_trace_id_middleware_new() {
        let _mw = TraceIdMiddleware::new();
    }

    #[test]
    fn test_trace_id_middleware_default() {
        let _mw: TraceIdMiddleware = Default::default();
    }
}
