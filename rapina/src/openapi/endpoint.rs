//! OpenAPI endpoint for exposing the API specification

use std::sync::Arc;

use http::{Request, Response, StatusCode, header::CONTENT_TYPE};
use hyper::body::Incoming;

use crate::{
    extract::PathParams,
    openapi::OpenApiSpec,
    response::{APPLICATION_JSON, BoxBody, full},
    state::AppState,
};

/// Registry for storing the OpenAPI spec
#[derive(Debug, Clone)]
pub struct OpenApiRegistry {
    spec: OpenApiSpec,
}

impl OpenApiRegistry {
    pub fn new(spec: OpenApiSpec) -> Self {
        Self { spec }
    }

    pub fn spec(&self) -> &OpenApiSpec {
        &self.spec
    }
}

/// Handler for the OpenAPI endpoint
///
/// Returns the OpenAPI specification as JSON
pub async fn openapi_spec(
    _req: Request<Incoming>,
    _params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    let registry = state.get::<OpenApiRegistry>();

    match registry {
        Some(registry) => {
            let json = serde_json::to_vec_pretty(registry.spec()).unwrap_or_default();
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, APPLICATION_JSON)
                .body(full(json))
                .unwrap()
        }
        None => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .header(CONTENT_TYPE, APPLICATION_JSON)
            .body(full(bytes::Bytes::from_static(
                br#"{"error": "OpenAPI spec not configured"}"#,
            )))
            .unwrap(),
    }
}

#[cfg(test)]
mod tests {
    use http::{HeaderValue, Method, StatusCode};
    use serde_json::Value;

    use crate::{app::Rapina, router::Router, testing::TestClient};

    #[tokio::test]
    async fn test_openapi_spec_returns_200_with_json_content_type() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router).openapi("openapi-test", "1.0");
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/openapi.json").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(http::header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
    }

    #[tokio::test]
    async fn test_openapi_spec_returns_valid_openapi_json_structure() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router).openapi("openapi-test", "1.0");
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/openapi.json").send().await;
        let json = response.json::<Value>();

        assert!(json.get("openapi").is_some());
        assert!(json.get("info").is_some());
        assert!(json.get("paths").is_some());
        assert!(json.get("components").is_some());
    }

    #[tokio::test]
    async fn test_openapi_spec_returns_404_and_empty_body_when_openapi_is_disabled() {
        let router = Router::new().route(Method::GET, "/hello", |_, _, _| async { "hello" });
        let app = Rapina::new().router(router);
        let client = TestClient::new(app).await;
        let response = client.get("/__rapina/openapi.json").send().await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(response.text().is_empty());
    }
}
