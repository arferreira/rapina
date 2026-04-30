//! Test client for integration testing Rapina applications.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::Bytes;
use http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper::body::Incoming;
use hyper::service::service_fn;
use hyper_util::client::legacy::Client;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto;
use serde::{Serialize, de::DeserializeOwned};
use tokio::net::TcpListener;
use tokio::sync::oneshot;

use crate::context::RequestContext;
use crate::middleware::MiddlewareStack;
use crate::response::{APPLICATION_JSON, FORM_CONTENT_TYPE};
use crate::router::Router;
use crate::state::AppState;

/// A test client for making HTTP requests to a Rapina application.
///
/// The test client spawns a lightweight HTTP server on a random port
/// and provides a convenient API for making requests and asserting responses.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
/// use rapina::testing::TestClient;
///
/// #[tokio::test]
/// async fn test_hello() {
///     let app = Rapina::new()
///         .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "Hello!" }));
///
///     let client = TestClient::new(app).await;
///     let response = client.get("/").send().await;
///
///     assert_eq!(response.status(), StatusCode::OK);
///     assert_eq!(response.text(), "Hello!");
/// }
/// ```
pub struct TestClient {
    addr: SocketAddr,
    client: Client<hyper_util::client::legacy::connect::HttpConnector, Full<Bytes>>,
    _shutdown: oneshot::Sender<()>,
}

impl TestClient {
    /// Creates a new test client from a Rapina application.
    ///
    /// This spawns a background server on a random available port.
    pub async fn new(app: crate::app::Rapina) -> Self {
        let app = app.prepare();
        Self::from_parts(app.router, app.state, app.middlewares).await
    }

    /// Creates a test client from router, state, and middlewares.
    pub async fn from_parts(router: Router, state: AppState, middlewares: MiddlewareStack) -> Self {
        let router = Arc::new(router);
        let state = Arc::new(state);
        let middlewares = Arc::new(middlewares);

        // Bind to a random available port
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

        // Spawn the server
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = listener.accept() => {
                        match result {
                            Ok((stream, _)) => {
                                let io = TokioIo::new(stream);
                                let router = router.clone();
                                let state = state.clone();
                                let middlewares = middlewares.clone();

                                tokio::spawn(async move {
                                    let service = service_fn(move |mut req: Request<Incoming>| {
                                        let router = router.clone();
                                        let state = state.clone();
                                        let middlewares = middlewares.clone();

                                        let ctx = RequestContext::new();
                                        req.extensions_mut().insert(ctx.clone());

                                        async move {
                                            let response = middlewares.execute(req, router, state, &ctx).await;
                                            Ok::<_, std::convert::Infallible>(response)
                                        }
                                    });

                                    let _ = auto::Builder::new(TokioExecutor::new())
                                        .serve_connection_with_upgrades(io, service)
                                        .into_owned()
                                        .await;
                                });
                            }
                            Err(_) => break,
                        }
                    }
                    _ = &mut shutdown_rx => {
                        break;
                    }
                }
            }
        });

        let client = Client::builder(hyper_util::rt::TokioExecutor::new()).build_http();

        Self {
            addr,
            client,
            _shutdown: shutdown_tx,
        }
    }

    /// Creates a GET request builder.
    pub fn get(&self, path: &str) -> TestRequestBuilder<'_> {
        self.request(Method::GET, path)
    }

    /// Creates a POST request builder.
    pub fn post(&self, path: &str) -> TestRequestBuilder<'_> {
        self.request(Method::POST, path)
    }

    /// Creates a PUT request builder.
    pub fn put(&self, path: &str) -> TestRequestBuilder<'_> {
        self.request(Method::PUT, path)
    }

    /// Creates a DELETE request builder.
    pub fn delete(&self, path: &str) -> TestRequestBuilder<'_> {
        self.request(Method::DELETE, path)
    }

    /// Creates a PATCH request builder.
    pub fn patch(&self, path: &str) -> TestRequestBuilder<'_> {
        self.request(Method::PATCH, path)
    }

    /// Creates a request builder with the given method and path.
    pub fn request(&self, method: Method, path: &str) -> TestRequestBuilder<'_> {
        TestRequestBuilder::new(self, method, path)
    }

    /// Returns the address the test server is listening on.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Builder for constructing test requests.
pub struct TestRequestBuilder<'a> {
    client: &'a TestClient,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
}

impl<'a> TestRequestBuilder<'a> {
    fn new(client: &'a TestClient, method: Method, path: &str) -> Self {
        Self {
            client,
            method,
            path: path.to_string(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
        }
    }

    /// Adds a header to the request.
    pub fn header(mut self, key: &str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::from_bytes(key.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        self
    }

    /// Sets a JSON body on the request.
    pub fn json<T: Serialize>(mut self, body: &T) -> Self {
        self.body = Bytes::from(serde_json::to_vec(body).unwrap());
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static(APPLICATION_JSON),
        );
        self
    }

    /// Sets a form body on the request.
    pub fn form<T: Serialize>(mut self, body: &T) -> Self {
        self.body = Bytes::from(serde_urlencoded::to_string(body).unwrap());
        self.headers.insert(
            http::header::CONTENT_TYPE,
            HeaderValue::from_static(FORM_CONTENT_TYPE),
        );
        self
    }

    /// Sets raw body bytes.
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    /// Sends the request and returns the response.
    pub async fn send(self) -> TestResponse {
        let uri = format!("http://{}{}", self.client.addr, self.path);

        let mut builder = Request::builder().method(self.method).uri(&uri);

        for (key, value) in self.headers.iter() {
            builder = builder.header(key, value);
        }

        let request = builder.body(Full::new(self.body)).unwrap();

        let response = self.client.client.request(request).await.unwrap();

        let status = response.status();
        let headers = response.headers().clone();
        let body = response.into_body().collect().await.unwrap().to_bytes();

        TestResponse {
            status,
            headers,
            body,
        }
    }
}

/// Response from a test request.
pub struct TestResponse {
    status: StatusCode,
    headers: HeaderMap,
    body: Bytes,
}

impl TestResponse {
    /// Returns the HTTP status code.
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// Returns the response headers.
    pub fn headers(&self) -> &HeaderMap {
        &self.headers
    }

    /// Returns the response body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// Returns the response body as raw bytes.
    pub fn bytes(&self) -> &Bytes {
        &self.body
    }

    /// Deserializes the response body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// Attempts to deserialize the response body as JSON.
    pub fn try_json<T: DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_slice(&self.body)
    }

    /// Asserts the response body matches a saved snapshot.
    ///
    /// In bless mode (`RAPINA_BLESS=1`), saves the response as a new snapshot.
    /// Otherwise, compares against the existing snapshot and panics on mismatch.
    ///
    /// Dynamic values (UUIDs, timestamps) are automatically redacted.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let response = client.get("/users/1").send().await;
    /// response.assert_snapshot("get_user_by_id");
    /// ```
    pub fn assert_snapshot(&self, name: &str) {
        let content_type = self
            .headers
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/octet-stream");
        super::snapshot::assert_snapshot(name, self.status.as_u16(), content_type, &self.body);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Rapina;

    #[tokio::test]
    async fn test_client_get() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(Router::new().route(http::Method::GET, "/", |_, _, _| async { "Hello!" }));

        let client = TestClient::new(app).await;
        let response = client.get("/").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text(), "Hello!");
    }

    #[tokio::test]
    async fn test_client_post_json() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(http::Method::POST, "/echo", |req, _, _| async move {
                    use http_body_util::BodyExt;
                    let body = req.into_body().collect().await.unwrap().to_bytes();
                    String::from_utf8_lossy(&body).to_string()
                }),
            );

        let client = TestClient::new(app).await;
        let response = client
            .post("/echo")
            .json(&serde_json::json!({"name": "test"}))
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.text().contains("test"));
    }

    #[tokio::test]
    async fn test_client_with_headers() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(http::Method::GET, "/headers", |req, _, _| async move {
                    let auth = req
                        .headers()
                        .get("authorization")
                        .map(|v| v.to_str().unwrap_or(""))
                        .unwrap_or("");
                    auth.to_string()
                }),
            );

        let client = TestClient::new(app).await;
        let response = client
            .get("/headers")
            .header("authorization", "Bearer token123")
            .send()
            .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text(), "Bearer token123");
    }

    #[tokio::test]
    async fn test_client_not_found() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(Router::new());

        let client = TestClient::new(app).await;
        let response = client.get("/nonexistent").send().await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_client_json_response() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(http::Method::GET, "/json", |_, _, _| async {
                    http::Response::builder()
                        .status(StatusCode::OK)
                        .header(http::header::CONTENT_TYPE, APPLICATION_JSON)
                        .body(crate::response::full(bytes::Bytes::from_static(
                            br#"{"id":1,"name":"test"}"#,
                        )))
                        .unwrap()
                }),
            );

        let client = TestClient::new(app).await;
        let response = client.get("/json").send().await;

        assert_eq!(response.status(), StatusCode::OK);

        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Data {
            id: i32,
            name: String,
        }

        let data: Data = response.json();
        assert_eq!(data.id, 1);
        assert_eq!(data.name, "test");
    }

    #[tokio::test]
    async fn test_client_with_state() {
        use std::sync::Arc;

        #[derive(Clone)]
        struct AppConfig {
            name: String,
        }

        let app = Rapina::new()
            .with_introspection(false)
            .state(AppConfig {
                name: "TestApp".to_string(),
            })
            .router(Router::new().route(
                http::Method::GET,
                "/config",
                |_, _, state: Arc<AppState>| async move {
                    let config = state.get::<AppConfig>().unwrap();
                    config.name.clone()
                },
            ));

        let client = TestClient::new(app).await;
        let response = client.get("/config").send().await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.text(), "TestApp");
    }

    #[tokio::test]
    async fn test_client_put() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(Method::PUT, "/resource", |_, _, _| async {
                    StatusCode::NO_CONTENT
                }),
            );

        let client = TestClient::new(app).await;
        let response = client.put("/resource").send().await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_client_delete() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(Method::DELETE, "/resource", |_, _, _| async {
                    StatusCode::NO_CONTENT
                }),
            );

        let client = TestClient::new(app).await;
        let response = client.delete("/resource").send().await;

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn test_response_bytes() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(
                Router::new().route(http::Method::GET, "/bytes", |_, _, _| async { "raw bytes" }),
            );

        let client = TestClient::new(app).await;
        let response = client.get("/bytes").send().await;

        assert_eq!(response.bytes(), &Bytes::from("raw bytes"));
    }

    #[tokio::test]
    async fn test_client_addr() {
        let app = Rapina::new()
            .with_introspection(false)
            .router(Router::new());

        let client = TestClient::new(app).await;
        let addr = client.addr();

        assert!(addr.port() > 0);
        assert_eq!(addr.ip().to_string(), "127.0.0.1");
    }
}
