//! Route auto-discovery via `inventory`.
//!
//! When handlers are annotated with `#[get]`, `#[post]`, `#[put]`, or `#[delete]`,
//! the macro emits an `inventory::submit!` that registers a [`RouteDescriptor`]
//! at link time. Calling [`Rapina::discover()`](crate::app::Rapina::discover)
//! iterates these descriptors and wires them into the router automatically.
//!
//! Use the `group` parameter to nest discovered routes under a prefix:
//!
//! ```ignore
//! #[get("/users", group = "/api")]
//! async fn list_users() -> Json<Vec<User>> { /* ... */ }
//! // registers at /api/users
//! ```

use serde::Serialize;

use crate::error::ErrorVariant;
use crate::router::Router;

/// Metadata about a typed header parameter declared on a route handler.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct HeaderParamInfo {
    /// The HTTP header name (e.g. "X-Request-Id").
    pub name: String,
    /// Whether the header is required (`Header<T>`) or optional (`Option<Header<T>>`).
    pub required: bool,
}

/// Metadata about a route handler, collected at link time via `inventory`.
///
/// Emitted by `#[get]`, `#[post]`, `#[put]`, `#[delete]` macros.
pub struct RouteDescriptor {
    /// HTTP method (GET, POST, PUT, DELETE)
    pub method: &'static str,
    /// Route path pattern (e.g. "/users/:id")
    pub path: &'static str,
    /// Function name of the handler
    pub handler_name: &'static str,
    /// Whether the handler is marked `#[public]`
    pub is_public: bool,
    /// Returns the JSON Schema for the response type, if available
    pub response_schema: fn() -> Option<serde_json::Value>,
    /// Returns the JSON Schema for the request body, if available
    pub request_schema: fn() -> Option<serde_json::Value>,
    /// Returns the content type for the request body (e.g., "application/json")
    pub request_content_type: fn() -> Option<&'static str>,
    /// Returns whether the request body is required (true) or optional (false)
    pub request_body_required: fn() -> Option<bool>,
    /// Returns documented error variants for this route
    pub error_responses: fn() -> Vec<ErrorVariant>,
    /// Returns typed header parameters declared on this handler
    pub header_parameters: fn() -> Vec<HeaderParamInfo>,
    /// Returns a human-readable description of this handler, if any
    pub description: fn() -> Option<&'static str>,
    /// Registers this route on the given Router and returns it
    pub register: fn(Router) -> Router,
}

inventory::collect!(RouteDescriptor);

/// Metadata about a custom Prometheus collector, collected at link time via
/// `inventory`.
///
/// Emitted by `#[metric]` on a static collector. Registered into the metrics
/// registry when both `.enable_metrics()` and `.discover()` are set.
#[cfg(feature = "metrics")]
pub struct MetricDescriptor {
    /// Produces an owned, boxed clone of the collector.
    pub collector: fn() -> Box<dyn prometheus::core::Collector>,
}

#[cfg(feature = "metrics")]
inventory::collect!(MetricDescriptor);
