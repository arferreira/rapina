//! CORS (Cross-Origin Resource Sharing) middleware.
//!
//! Provides configurable CORS support for Rapina applications,
//! handling preflight OPTIONS requests and adding appropriate headers.

use http::{HeaderValue, Method, Request, Response, StatusCode, header};
use hyper::body::Incoming;

use crate::context::RequestContext;
use crate::response::BoxBody;

use super::{BoxFuture, Middleware, Next};

/// Configuration for CORS middleware.
///
/// Use `permissive()` for development or `with_origins()` for production.
#[derive(Debug, Clone)]
pub struct CorsConfig {
    /// Allowed origins for CORS requests.
    pub allowed_origins: AllowedOrigins,
    /// Allowed HTTP methods.
    pub allowed_methods: AllowedMethods,
    /// Allowed request headers.
    pub allowed_headers: AllowedHeaders,
}

impl CorsConfig {
    /// Creates a permissive CORS config that allows all origins, methods, and headers.
    ///
    /// Suitable for development. Do not use in production.
    pub fn permissive() -> Self {
        Self {
            allowed_origins: AllowedOrigins::Any,
            allowed_methods: AllowedMethods::Any,
            allowed_headers: AllowedHeaders::Any,
        }
    }

    /// Creates a CORS config with specific allowed origins.
    ///
    /// Uses sensible defaults for methods (GET, POST, PUT, PATCH, DELETE, OPTIONS)
    /// and headers (Accept, Authorization).
    pub fn with_origins(origins: Vec<String>) -> Self {
        Self {
            allowed_methods: AllowedMethods::List(vec![
                Method::GET,
                Method::POST,
                Method::PUT,
                Method::PATCH,
                Method::DELETE,
                Method::OPTIONS,
            ]),
            allowed_origins: AllowedOrigins::Exact(origins),
            allowed_headers: AllowedHeaders::List(vec![header::ACCEPT, header::AUTHORIZATION]),
        }
    }
}

/// Specifies which headers are allowed in CORS requests.
#[derive(Debug, Clone)]
pub enum AllowedHeaders {
    /// Allow any headers.
    Any,
    /// Allow only specific headers.
    List(Vec<header::HeaderName>),
}

/// Specifies which HTTP methods are allowed in CORS requests.
#[derive(Debug, Clone)]
pub enum AllowedMethods {
    /// Allow any method.
    Any,
    /// Allow only specific methods.
    List(Vec<Method>),
}

/// Specifies which origins are allowed for CORS requests.
#[derive(Debug, Clone)]
pub enum AllowedOrigins {
    /// Allow any origin (`*`).
    Any,
    /// Allow only specific origins.
    Exact(Vec<String>),
}

/// Middleware that handles CORS headers and preflight requests.
#[derive(Debug, Clone)]
pub struct CorsMiddleware {
    config: CorsConfig,
}

impl CorsMiddleware {
    /// Creates a new CORS middleware with the given configuration.
    pub fn new(config: CorsConfig) -> Self {
        Self { config }
    }

    fn preflight_response(&self, origin: &Option<HeaderValue>) -> Response<BoxBody> {
        let mut builder = Response::builder().status(StatusCode::NO_CONTENT);

        // Set Access-Control-Allow-Origin
        match &self.config.allowed_origins {
            AllowedOrigins::Any => {
                builder = builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*");
            }
            AllowedOrigins::Exact(origins) => {
                if let Some(req_origin) = origin {
                    let origin_str = req_origin.to_str().unwrap_or("");
                    if origins.iter().any(|o| o == origin_str) {
                        builder =
                            builder.header(header::ACCESS_CONTROL_ALLOW_ORIGIN, req_origin.clone());
                    }
                }
            }
        }

        // Set Access-Control-Allow-Methods
        let methods_value = match &self.config.allowed_methods {
            AllowedMethods::Any => "*".to_string(),
            AllowedMethods::List(methods) => methods
                .iter()
                .map(|m| m.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        };
        builder = builder.header(header::ACCESS_CONTROL_ALLOW_METHODS, methods_value);

        // Set Access-Control-Allow-Headers
        let headers_value = match &self.config.allowed_headers {
            AllowedHeaders::Any => "*".to_string(),
            AllowedHeaders::List(headers) => headers
                .iter()
                .map(|h| h.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        };
        builder = builder.header(header::ACCESS_CONTROL_ALLOW_HEADERS, headers_value);

        builder = builder.header(header::VARY, "Origin");

        builder.body(crate::response::empty()).unwrap()
    }

    fn add_cors_headers(&self, response: &mut Response<BoxBody>, origin: &Option<HeaderValue>) {
        let headers = response.headers_mut();

        // Set Access-Control-Allow-Origin
        match &self.config.allowed_origins {
            AllowedOrigins::Any => {
                headers.insert(
                    header::ACCESS_CONTROL_ALLOW_ORIGIN,
                    HeaderValue::from_static("*"),
                );
            }
            AllowedOrigins::Exact(origins) => {
                if let Some(req_origin) = origin {
                    let origin_str = req_origin.to_str().unwrap_or("");
                    if origins.iter().any(|o| o == origin_str) {
                        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, req_origin.clone());
                    }
                }
            }
        }

        // Vary header
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
    }
}

impl Middleware for CorsMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let origin = req.headers().get(header::ORIGIN).cloned();

            // if it's OPTIONS (preflight), return early with 204 + CORS headers
            if req.method() == Method::OPTIONS {
                return self.preflight_response(&origin);
            }

            let mut response = next.run(req).await;
            self.add_cors_headers(&mut response, &origin);
            response
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{HeaderValue, Method, StatusCode, header};

    // --- Config constructors ---

    #[test]
    fn test_permissive_config() {
        let config = CorsConfig::permissive();
        assert!(matches!(config.allowed_origins, AllowedOrigins::Any));
        assert!(matches!(config.allowed_methods, AllowedMethods::Any));
        assert!(matches!(config.allowed_headers, AllowedHeaders::Any));
    }

    #[test]
    fn test_with_origins_config() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        match &config.allowed_origins {
            AllowedOrigins::Exact(origins) => {
                assert_eq!(origins, &vec!["https://example.com".to_string()]);
            }
            _ => panic!("expected Exact origins"),
        }
        match &config.allowed_methods {
            AllowedMethods::List(methods) => {
                assert_eq!(methods.len(), 6);
                assert!(methods.contains(&Method::GET));
                assert!(methods.contains(&Method::POST));
                assert!(methods.contains(&Method::PUT));
                assert!(methods.contains(&Method::PATCH));
                assert!(methods.contains(&Method::DELETE));
                assert!(methods.contains(&Method::OPTIONS));
            }
            _ => panic!("expected List methods"),
        }
        match &config.allowed_headers {
            AllowedHeaders::List(headers) => {
                assert_eq!(headers.len(), 2);
                assert!(headers.contains(&header::ACCEPT));
                assert!(headers.contains(&header::AUTHORIZATION));
            }
            _ => panic!("expected List headers"),
        }
    }

    // --- Preflight response ---

    #[test]
    fn test_preflight_returns_204() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let resp = mw.preflight_response(&None);
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    }

    #[test]
    fn test_preflight_wildcard_origin() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let resp = mw.preflight_response(&None);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn test_preflight_allowed_origin_echoed() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let origin = Some(HeaderValue::from_static("https://example.com"));
        let resp = mw.preflight_response(&origin);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn test_preflight_disallowed_origin() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let origin = Some(HeaderValue::from_static("https://evil.com"));
        let resp = mw.preflight_response(&origin);
        assert!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[test]
    fn test_preflight_no_origin_header() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let resp = mw.preflight_response(&None);
        assert!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[test]
    fn test_preflight_methods_any() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let resp = mw.preflight_response(&None);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_METHODS)
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn test_preflight_methods_list() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let origin = Some(HeaderValue::from_static("https://example.com"));
        let resp = mw.preflight_response(&origin);
        let methods = resp
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_METHODS)
            .unwrap()
            .to_str()
            .unwrap();
        for m in ["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
            assert!(methods.contains(m), "missing method: {m}");
        }
    }

    #[test]
    fn test_preflight_headers_any() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let resp = mw.preflight_response(&None);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn test_preflight_headers_list() {
        let config = CorsConfig::with_origins(vec!["https://x.com".into()]);
        let mw = CorsMiddleware::new(config);
        let origin = Some(HeaderValue::from_static("https://x.com"));
        let resp = mw.preflight_response(&origin);
        let headers_val = resp
            .headers()
            .get(header::ACCESS_CONTROL_ALLOW_HEADERS)
            .unwrap()
            .to_str()
            .unwrap();
        assert!(headers_val.contains("accept"), "missing accept header");
        assert!(
            headers_val.contains("authorization"),
            "missing authorization header"
        );
    }

    #[test]
    fn test_preflight_vary_header() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let resp = mw.preflight_response(&None);
        assert_eq!(resp.headers().get(header::VARY).unwrap(), "Origin");
    }

    // --- add_cors_headers on normal responses ---

    fn empty_response() -> Response<BoxBody> {
        Response::builder()
            .status(StatusCode::OK)
            .body(crate::response::empty())
            .unwrap()
    }

    #[test]
    fn test_normal_response_wildcard_origin() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let mut resp = empty_response();
        mw.add_cors_headers(&mut resp, &None);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "*"
        );
    }

    #[test]
    fn test_normal_response_allowed_origin() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let mut resp = empty_response();
        let origin = Some(HeaderValue::from_static("https://example.com"));
        mw.add_cors_headers(&mut resp, &origin);
        assert_eq!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn test_normal_response_disallowed_origin() {
        let config = CorsConfig::with_origins(vec!["https://example.com".into()]);
        let mw = CorsMiddleware::new(config);
        let mut resp = empty_response();
        let origin = Some(HeaderValue::from_static("https://evil.com"));
        mw.add_cors_headers(&mut resp, &origin);
        assert!(
            resp.headers()
                .get(header::ACCESS_CONTROL_ALLOW_ORIGIN)
                .is_none()
        );
    }

    #[test]
    fn test_normal_response_vary_header() {
        let mw = CorsMiddleware::new(CorsConfig::permissive());
        let mut resp = empty_response();
        mw.add_cors_headers(&mut resp, &None);
        assert_eq!(resp.headers().get(header::VARY).unwrap(), "Origin");
    }
}
