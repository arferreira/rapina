//! Introspection endpoint for exposing route metadata.

use std::sync::Arc;

use http::{Request, Response, StatusCode, header::CONTENT_TYPE};
use hyper::body::Incoming;

use crate::extract::PathParams;
use crate::introspection::RouteInfo;
use crate::response::{APPLICATION_JSON, BoxBody, IntoResponse, full};
use crate::state::AppState;

/// Registry of route information stored in application state.
///
/// This is automatically populated when introspection is enabled
/// and can be accessed by the introspection endpoint.
#[derive(Debug, Clone, Default)]
pub struct RouteRegistry {
    routes: Vec<RouteInfo>,
}

impl RouteRegistry {
    /// Creates a new empty route registry.
    pub fn new() -> Self {
        Self { routes: Vec::new() }
    }

    /// Creates a route registry with the given routes.
    pub fn with_routes(routes: Vec<RouteInfo>) -> Self {
        Self { routes }
    }

    /// Returns the registered routes.
    pub fn routes(&self) -> &[RouteInfo] {
        &self.routes
    }
}

/// Handler for the introspection endpoint.
///
/// Returns all registered routes as JSON.
pub async fn list_routes(
    _req: Request<Incoming>,
    _params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    let registry = state.get::<RouteRegistry>();

    match registry {
        Some(registry) => {
            let json = serde_json::to_vec(registry.routes()).unwrap_or_default();
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, APPLICATION_JSON)
                .body(full(json))
                .unwrap()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, Method};
    use serde_json::Value;

    use crate::{app::Rapina, router::Router, testing::TestClient};

    use super::*;

    #[test]
    fn test_route_registry_new() {
        let registry = RouteRegistry::new();
        assert!(registry.routes().is_empty());
    }

    #[test]
    fn test_route_registry_default() {
        let registry = RouteRegistry::default();
        assert!(registry.routes().is_empty());
    }

    #[test]
    fn test_route_registry_with_routes() {
        let routes = vec![
            RouteInfo::new(
                "GET",
                "/users",
                "list_users",
                None,
                None,
                None::<String>,
                None,
                Vec::new(),
            ),
            RouteInfo::new(
                "POST",
                "/users",
                "create_user",
                None,
                None,
                None::<String>,
                None,
                Vec::new(),
            ),
        ];
        let registry = RouteRegistry::with_routes(routes);
        assert_eq!(registry.routes().len(), 2);
    }

    #[test]
    fn test_route_registry_clone() {
        let routes = vec![RouteInfo::new(
            "GET",
            "/",
            "index",
            None,
            None,
            None::<String>,
            None,
            Vec::new(),
        )];
        let registry = RouteRegistry::with_routes(routes);
        let cloned = registry.clone();
        assert_eq!(registry.routes().len(), cloned.routes().len());
    }

    #[test]
    fn test_route_registry_routes_content() {
        let routes = vec![
            RouteInfo::new(
                "GET",
                "/health",
                "health_check",
                None,
                None,
                None::<String>,
                None,
                Vec::new(),
            ),
            RouteInfo::new(
                "POST",
                "/users",
                "create_user",
                None,
                None,
                None::<String>,
                None,
                Vec::new(),
            ),
        ];
        let registry = RouteRegistry::with_routes(routes);

        assert_eq!(registry.routes()[0].method, "GET");
        assert_eq!(registry.routes()[0].path, "/health");
        assert_eq!(registry.routes()[0].handler_name, "health_check");
        assert_eq!(registry.routes()[1].method, "POST");
    }

    #[test]
    fn test_route_registry_debug() {
        let registry = RouteRegistry::new();
        let debug = format!("{:?}", registry);
        assert!(debug.contains("RouteRegistry"));
    }

    #[tokio::test]
    async fn test_list_routes_returns_200_with_json_content_type() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router).with_introspection(true);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/routes").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(http::header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
    }

    #[tokio::test]
    async fn test_list_routes_response_contains_registered_routes() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router).with_introspection(true);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/routes").send().await;
        let json = response.json::<Value>();

        assert!(json.is_array());

        let route = &json[0];

        assert_eq!(route["method"], "GET");
        assert_eq!(route["path"], "/hello");
        assert_eq!(route["handler_name"], "handler");
    }

    #[tokio::test]
    async fn test_list_routes_returns_404_and_empty_body_when_with_introspection_disabled() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router).with_introspection(false);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/routes").send().await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
}
