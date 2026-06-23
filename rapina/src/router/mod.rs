//! HTTP routing for Rapina applications.
//!
//! The [`Router`] type collects route definitions and matches incoming
//! requests to the appropriate handlers.

mod static_map;
mod trie;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use http::{Method, Request, Response, StatusCode};
use hyper::body::Incoming;

use crate::context::{MatchedPattern, RequestContext};
use crate::discovery::HeaderParamInfo;
use crate::error::ErrorVariant;
use crate::extract::PathParams;
use crate::handler::Handler;
use crate::introspection::RouteInfo;
use crate::response::{BoxBody, IntoResponse};
use crate::state::AppState;

type BoxFuture = Pin<Box<dyn Future<Output = Response<BoxBody>> + Send>>;
type HandlerFn =
    Box<dyn Fn(Request<Incoming>, PathParams, Arc<AppState>) -> BoxFuture + Send + Sync>;

/// Configuration for a route including metadata for introspection.
pub struct RouteConfig {
    /// The handler name for introspection and documentation.
    pub handler_name: String,
    /// JSON schema for the response body.
    pub response_schema: Option<serde_json::Value>,
    /// JSON schema for the request body.
    pub request_schema: Option<serde_json::Value>,
    /// Content type for the request body.
    pub request_content_type: Option<&'static str>,
    /// Whether the request body is required (true) or optional (false).
    pub request_body_required: Option<bool>,
    /// Error responses this handler may return.
    pub error_responses: Vec<ErrorVariant>,
    /// Typed header parameters declared on this handler.
    pub header_parameters: Vec<HeaderParamInfo>,
    /// Human-readable description of what this handler does.
    pub description: Option<&'static str>,
}

impl Default for RouteConfig {
    fn default() -> Self {
        Self {
            handler_name: "handler".to_string(),
            response_schema: None,
            request_schema: None,
            request_content_type: None,
            request_body_required: None,
            error_responses: Vec::new(),
            header_parameters: Vec::new(),
            description: None,
        }
    }
}

pub(crate) struct Route {
    pub(crate) pattern: String,
    pub(crate) handler_name: String,
    pub(crate) response_schema: Option<serde_json::Value>,
    pub(crate) request_schema: Option<serde_json::Value>,
    pub(crate) request_content_type: Option<&'static str>,
    pub(crate) request_body_required: Option<bool>,
    pub(crate) error_responses: Vec<ErrorVariant>,
    pub(crate) header_parameters: Vec<HeaderParamInfo>,
    pub(crate) description: Option<&'static str>,
    handler: HandlerFn,
}

/// The HTTP router for matching requests to handlers.
///
/// Static routes (no `:param` segments) are resolved via O(1) HashMap
/// lookup. Dynamic routes are matched through a radix trie with
/// O(path_depth) complexity. Static children take precedence over
/// param children at every node, so `/users/current` always wins
/// over `/users/:id` regardless of registration order.
///
/// # Examples
///
/// ```
/// use rapina::prelude::*;
///
/// #[get("/")]
/// async fn hello() -> &'static str { "Hello!" }
///
/// #[get("/users/:id")]
/// async fn get_user() -> &'static str { "User" }
///
/// #[post("/users")]
/// async fn create_user() -> StatusCode { StatusCode::CREATED }
///
/// let router = Router::new()
///     .get("/", hello)
///     .get("/users/:id", get_user)
///     .post("/users", create_user);
/// ```
pub struct Router {
    pub(crate) routes: Vec<(Method, Route)>,
    static_map: Option<static_map::StaticMap>,
    trie: Option<trie::TrieRouter>,
}

impl Router {
    /// Creates a new empty router.
    pub fn new() -> Self {
        Self {
            routes: Vec::new(),
            static_map: None,
            trie: None,
        }
    }

    /// Adds a route with the given HTTP method, pattern, and configuration.
    ///
    /// The handler name is used for route introspection and documentation.
    pub fn route_named<F, Fut, Out>(
        mut self,
        method: Method,
        pattern: &str,
        config: RouteConfig,
        handler: F,
    ) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        let handler = Box::new(
            move |req: Request<Incoming>, params: PathParams, state: Arc<AppState>| {
                let handler = handler.clone();
                Box::pin(async move {
                    let output = handler(req, params, state).await;
                    output.into_response()
                }) as BoxFuture
            },
        );

        let route = Route {
            pattern: pattern.to_string(),
            handler_name: config.handler_name,
            response_schema: config.response_schema,
            request_schema: config.request_schema,
            request_content_type: config.request_content_type,
            request_body_required: config.request_body_required,
            error_responses: config.error_responses,
            header_parameters: config.header_parameters,
            description: config.description,
            handler,
        };

        self.routes.push((method, route));
        self
    }

    /// Adds a route with the given HTTP method and pattern.
    ///
    /// The handler name defaults to "handler". Use [`route_named`](Self::route_named)
    /// to specify a custom handler name for introspection.
    pub fn route<F, Fut, Out>(self, method: Method, pattern: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(method, pattern, RouteConfig::default(), handler)
    }

    /// Adds a GET route with a handler name.
    pub fn get_named<F, Fut, Out>(self, pattern: &str, handler_name: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(
            Method::GET,
            pattern,
            RouteConfig {
                handler_name: handler_name.to_string(),
                ..Default::default()
            },
            handler,
        )
    }

    /// Adds a POST route with a handler name.
    pub fn post_named<F, Fut, Out>(self, pattern: &str, handler_name: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(
            Method::POST,
            pattern,
            RouteConfig {
                handler_name: handler_name.to_string(),
                ..Default::default()
            },
            handler,
        )
    }

    /// Adds a GET route with a Handler.
    pub fn get<H: Handler>(self, pattern: &str, handler: H) -> Self {
        self.route_named(
            Method::GET,
            pattern,
            RouteConfig {
                handler_name: H::NAME.to_string(),
                response_schema: H::response_schema(),
                request_schema: H::request_schema(),
                request_content_type: H::request_content_type(),
                request_body_required: H::request_body_required(),
                error_responses: H::error_responses(),
                header_parameters: H::header_parameters(),
                description: H::description(),
            },
            move |req, params, state| {
                let h = handler.clone();
                async move { h.call(req, params, state).await }
            },
        )
    }

    /// Adds a POST route with a Handler.
    pub fn post<H: Handler>(self, pattern: &str, handler: H) -> Self {
        self.route_named(
            Method::POST,
            pattern,
            RouteConfig {
                handler_name: H::NAME.to_string(),
                response_schema: H::response_schema(),
                request_schema: H::request_schema(),
                request_content_type: H::request_content_type(),
                request_body_required: H::request_body_required(),
                error_responses: H::error_responses(),
                header_parameters: H::header_parameters(),
                description: H::description(),
            },
            move |req, params, state| {
                let h = handler.clone();
                async move { h.call(req, params, state).await }
            },
        )
    }

    /// Adds a PUT route with a handler name.
    pub fn put_named<F, Fut, Out>(self, pattern: &str, handler_name: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(
            Method::PUT,
            pattern,
            RouteConfig {
                handler_name: handler_name.to_string(),
                ..Default::default()
            },
            handler,
        )
    }

    /// Adds a PUT route with a Handler.
    pub fn put<H: Handler>(self, pattern: &str, handler: H) -> Self {
        self.route_named(
            Method::PUT,
            pattern,
            RouteConfig {
                handler_name: H::NAME.to_string(),
                response_schema: H::response_schema(),
                request_schema: H::request_schema(),
                request_content_type: H::request_content_type(),
                request_body_required: H::request_body_required(),
                error_responses: H::error_responses(),
                header_parameters: H::header_parameters(),
                description: H::description(),
            },
            move |req, params, state| {
                let h = handler.clone();
                async move { h.call(req, params, state).await }
            },
        )
    }

    /// Adds a PATCH route with a handler name.
    pub fn patch_named<F, Fut, Out>(self, pattern: &str, handler_name: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(
            Method::PATCH,
            pattern,
            RouteConfig {
                handler_name: handler_name.to_string(),
                ..Default::default()
            },
            handler,
        )
    }

    /// Adds a PATCH route with a Handler.
    pub fn patch<H: Handler>(self, pattern: &str, handler: H) -> Self {
        self.route_named(
            Method::PATCH,
            pattern,
            RouteConfig {
                handler_name: H::NAME.to_string(),
                response_schema: H::response_schema(),
                request_schema: H::request_schema(),
                request_content_type: H::request_content_type(),
                request_body_required: H::request_body_required(),
                error_responses: H::error_responses(),
                header_parameters: H::header_parameters(),
                description: H::description(),
            },
            move |req, params, state| {
                let h = handler.clone();
                async move { h.call(req, params, state).await }
            },
        )
    }

    /// Adds a DELETE route with a handler name.
    pub fn delete_named<F, Fut, Out>(self, pattern: &str, handler_name: &str, handler: F) -> Self
    where
        F: Fn(Request<Incoming>, PathParams, Arc<AppState>) -> Fut + Send + Sync + Clone + 'static,
        Fut: Future<Output = Out> + Send + 'static,
        Out: IntoResponse + 'static,
    {
        self.route_named(
            Method::DELETE,
            pattern,
            RouteConfig {
                handler_name: handler_name.to_string(),
                ..Default::default()
            },
            handler,
        )
    }

    /// Adds a DELETE route with a Handler.
    pub fn delete<H: Handler>(self, pattern: &str, handler: H) -> Self {
        self.route_named(
            Method::DELETE,
            pattern,
            RouteConfig {
                handler_name: H::NAME.to_string(),
                response_schema: H::response_schema(),
                request_schema: H::request_schema(),
                request_content_type: H::request_content_type(),
                request_body_required: H::request_body_required(),
                error_responses: H::error_responses(),
                header_parameters: H::header_parameters(),
                description: H::description(),
            },
            move |req, params, state| {
                let h = handler.clone();
                async move { h.call(req, params, state).await }
            },
        )
    }

    /// Returns metadata about all registered routes.
    ///
    /// This is useful for introspection, documentation generation,
    /// and AI-native tooling integration.
    ///
    /// # Examples
    ///
    /// ```
    /// use rapina::prelude::*;
    ///
    /// let router = Router::new()
    ///     .get_named("/users", "list_users", |_, _, _| async { "users" })
    ///     .post_named("/users", "create_user", |_, _, _| async { StatusCode::CREATED });
    ///
    /// let routes = router.routes();
    /// assert_eq!(routes.len(), 2);
    /// assert_eq!(routes[0].method, "GET");
    /// assert_eq!(routes[0].path, "/users");
    /// assert_eq!(routes[0].handler_name, "list_users");
    /// ```
    pub fn routes(&self) -> Vec<RouteInfo> {
        self.routes
            .iter()
            .map(|(method, route)| {
                RouteInfo::new(
                    method.as_str(),
                    &route.pattern,
                    &route.handler_name,
                    route.response_schema.clone(),
                    route.request_schema.clone(),
                    route.request_content_type,
                    route.request_body_required,
                    route.error_responses.clone(),
                    route.header_parameters.clone(),
                    route.description,
                )
            })
            .collect()
    }

    /// Adds all routes from another router with a path prefix to compose a group of endpoints.
    ///
    /// # Examples
    ///
    /// ```
    /// use rapina::prelude::*;
    ///
    /// let users_router = Router::new();
    ///
    /// let invoices_router = Router::new();
    ///
    /// let router = Router::new()
    ///     .group("/api/users", users_router)
    ///     .group("/api/invoices", invoices_router);
    /// ```
    pub fn group(mut self, prefix_pattern: &str, router: Router) -> Self {
        if !prefix_pattern.starts_with("/") {
            panic!("A group's prefix pattern must start with /");
        }

        for (method, mut route) in router.routes {
            let joined_route_path = Self::join_group_route_pattern(prefix_pattern, &route.pattern);
            route.pattern = joined_route_path;
            self.routes.push((method, route));
        }

        self
    }

    /// Resolves a route without calling the handler.
    ///
    /// Returns the matched route index and extracted path parameters,
    /// or `None` if no route matches. This is the pure routing decision
    /// isolated from the async handler call and HTTP plumbing.
    #[doc(hidden)]
    pub fn resolve(&self, method: &Method, path: &str) -> Option<(usize, PathParams)> {
        if let Some(ref static_map) = self.static_map {
            if let Some(idx) = static_map.lookup(method, path) {
                return Some((idx, PathParams::new()));
            }
        }
        if let Some(ref trie) = self.trie {
            let mut params = PathParams::new();
            if let Some(idx) = trie.lookup(method, path, &mut params) {
                return Some((idx, params));
            }
        }
        None
    }

    /// Resolves a route using the old linear scan (pre-trie) algorithm.
    ///
    /// Iterates over all registered routes checking each pattern against the
    /// request path. This is the O(n) baseline that the static map and trie
    /// replaced. Exposed only for benchmark comparison.
    #[doc(hidden)]
    pub fn resolve_linear(&self, method: &Method, path: &str) -> Option<(usize, PathParams)> {
        for (idx, (route_method, route)) in self.routes.iter().enumerate() {
            if *route_method != *method {
                continue;
            }
            if let Some(params) = crate::extract::extract_path_params(&route.pattern, path) {
                return Some((idx, params));
            }
        }
        None
    }

    /// Returns the route pattern that would match this method + path, without
    /// executing the handler.
    ///
    /// Used by middleware that needs to know the abstract template (e.g.
    /// `/users/:id`) rather than the concrete request path (e.g. `/users/42`).
    /// Returns `None` if no route matches (the request will 404 later).
    pub fn resolve_pattern(&self, method: &Method, path: &str) -> Option<&str> {
        self.resolve(method, path)
            .map(|(idx, _)| self.routes[idx].1.pattern.as_str())
    }

    /// Inserts the [`RequestContext`] and, when a route matches, the abstract
    /// [`MatchedPattern`] (e.g. `/users/:id`) into the request extensions.
    /// Shared by the live server and the test client so the two can't drift.
    pub(crate) fn prepare_request(&self, req: &mut Request<Incoming>) -> RequestContext {
        let ctx = RequestContext::new();
        req.extensions_mut().insert(ctx.clone());
        if let Some(pattern) = self.resolve_pattern(req.method(), req.uri().path()) {
            req.extensions_mut()
                .insert(MatchedPattern(pattern.to_string()));
        }
        ctx
    }

    /// Handles an incoming request by matching it to a route.
    pub async fn handle(&self, req: Request<Incoming>, state: &Arc<AppState>) -> Response<BoxBody> {
        // Layer 1: O(1) static map — no allocation, no cloning.
        if let Some(ref static_map) = self.static_map {
            if let Some(idx) = static_map.lookup(req.method(), req.uri().path()) {
                let route = &self.routes[idx].1;
                return (route.handler)(req, PathParams::new(), state.clone()).await;
            }
        }

        // Layer 2: radix trie for dynamic routes — no path allocation.
        if let Some(ref trie) = self.trie {
            let mut params = PathParams::new();
            if let Some(idx) = trie.lookup(req.method(), req.uri().path(), &mut params) {
                let route = &self.routes[idx].1;
                return (route.handler)(req, params, state.clone()).await;
            }
        }

        StatusCode::NOT_FOUND.into_response()
    }

    /// Sorts routes and builds lookup structures for benchmarking.
    ///
    /// Combines `sort_routes()` and `freeze()` into a single call accessible
    /// from benchmarks. Not part of the public API.
    #[doc(hidden)]
    pub fn prepare_bench(&mut self) {
        self.sort_routes();
        self.freeze();
    }

    /// Sorts routes so static segments come before parameterized ones.
    ///
    /// Route matching is handled by the static map and radix trie, which
    /// enforce static-before-param precedence structurally. This sort
    /// only affects the order of routes in introspection output and
    /// internal index numbering. Uses a stable sort so routes with
    /// identical specificity keep their original order.
    pub(crate) fn sort_routes(&mut self) {
        self.routes.sort_by(|(_, a), (_, b)| {
            route_specificity(&a.pattern).cmp(&route_specificity(&b.pattern))
        });
    }

    /// Builds the static route map and radix trie for fast route resolution.
    ///
    /// Called by `prepare()` after `sort_routes()`. After this, the router
    /// is frozen — no more routes can be added. Idempotent: calling this
    /// multiple times is safe and only builds the structures once.
    pub(crate) fn freeze(&mut self) {
        if self.static_map.is_some() {
            return;
        }
        self.static_map = Some(static_map::StaticMap::build(&self.routes));
        self.trie = Some(trie::TrieRouter::build(&self.routes));
    }

    fn join_group_route_pattern(prefix: &str, route_path: &str) -> String {
        let prefix = prefix.trim_end_matches('/');
        let route_path = route_path.trim_start_matches('/');

        if prefix.is_empty() {
            format!("/{}", route_path)
        } else if route_path.is_empty() {
            prefix.to_string()
        } else {
            format!("{}/{}", prefix, route_path)
        }
    }
}

/// Returns `true` if the pattern contains any `:param` segments.
pub(super) fn is_dynamic(pattern: &str) -> bool {
    pattern.split('/').any(|seg| seg.starts_with(':'))
}

/// Returns a specificity key for a route pattern.
///
/// Each segment maps to `0` (static) or `1` (`:param`). When sorted
/// ascending, static segments win over parameterized ones at every position,
/// so `/users/current` always comes before `/users/:id`.
fn route_specificity(pattern: &str) -> Vec<u8> {
    pattern
        .split('/')
        .map(|seg| if seg.starts_with(':') { 1 } else { 0 })
        .collect()
}

impl Default for Router {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_router_new() {
        let router = Router::new();
        assert!(router.routes.is_empty());
    }

    #[test]
    fn test_router_default() {
        let router = Router::default();
        assert!(router.routes.is_empty());
    }

    #[test]
    fn test_router_add_get_route() {
        let router = Router::new().route(Method::GET, "/users", |_req, _params, _state| async {
            StatusCode::OK
        });
        assert_eq!(router.routes.len(), 1);
        assert_eq!(router.routes[0].0, Method::GET);
        assert_eq!(router.routes[0].1.pattern, "/users");
    }

    #[test]
    fn test_router_add_post_route() {
        let router = Router::new().route(Method::POST, "/users", |_req, _params, _state| async {
            StatusCode::CREATED
        });
        assert_eq!(router.routes.len(), 1);
        assert_eq!(router.routes[0].0, Method::POST);
        assert_eq!(router.routes[0].1.pattern, "/users");
    }

    #[test]
    fn test_router_add_custom_method_route() {
        let router =
            Router::new().route(Method::PUT, "/users/:id", |_req, _params, _state| async {
                StatusCode::OK
            });
        assert_eq!(router.routes.len(), 1);
        assert_eq!(router.routes[0].0, Method::PUT);
        assert_eq!(router.routes[0].1.pattern, "/users/:id");
    }

    #[test]
    fn test_router_multiple_routes() {
        let router = Router::new()
            .route(Method::GET, "/users", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(Method::POST, "/users", |_req, _params, _state| async {
                StatusCode::CREATED
            })
            .route(
                Method::DELETE,
                "/users/:id",
                |_req, _params, _state| async { StatusCode::NO_CONTENT },
            );

        assert_eq!(router.routes.len(), 3);
        assert_eq!(router.routes[0].0, Method::GET);
        assert_eq!(router.routes[1].0, Method::POST);
        assert_eq!(router.routes[2].0, Method::DELETE);
    }

    #[test]
    fn test_router_chaining() {
        let router = Router::new()
            .route(Method::GET, "/", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(Method::GET, "/health", |_req, _params, _state| async {
                StatusCode::OK
            });

        assert_eq!(router.routes.len(), 2);
    }

    #[test]
    fn test_router_preserves_route_order() {
        let router = Router::new()
            .route(Method::GET, "/first", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(Method::GET, "/second", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(Method::GET, "/third", |_req, _params, _state| async {
                StatusCode::OK
            });

        assert_eq!(router.routes[0].1.pattern, "/first");
        assert_eq!(router.routes[1].1.pattern, "/second");
        assert_eq!(router.routes[2].1.pattern, "/third");
    }

    #[test]
    fn test_router_routes_introspection() {
        let router = Router::new()
            .get_named("/users", "list_users", |_req, _params, _state| async {
                StatusCode::OK
            })
            .post_named("/users", "create_user", |_req, _params, _state| async {
                StatusCode::CREATED
            });

        let routes = router.routes();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].handler_name, "list_users");
        assert_eq!(routes[1].method, "POST");
        assert_eq!(routes[1].path, "/users");
        assert_eq!(routes[1].handler_name, "create_user");
    }

    #[test]
    fn test_router_routes_default_handler_name() {
        let router = Router::new().route(Method::GET, "/health", |_req, _params, _state| async {
            StatusCode::OK
        });

        let routes = router.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].handler_name, "handler");
    }

    #[test]
    fn test_router_route_named() {
        let router = Router::new().route_named(
            Method::PUT,
            "/users/:id",
            RouteConfig {
                handler_name: "update_user".to_string(),
                ..Default::default()
            },
            |_req, _params, _state| async { StatusCode::OK },
        );

        let routes = router.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "PUT");
        assert_eq!(routes[0].path, "/users/:id");
        assert_eq!(routes[0].handler_name, "update_user");
    }

    #[test]
    fn test_router_get_named() {
        let router =
            Router::new().get_named("/items", "list_items", |_req, _params, _state| async {
                StatusCode::OK
            });

        let routes = router.routes();
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].handler_name, "list_items");
    }

    #[test]
    fn test_router_post_named() {
        let router =
            Router::new().post_named("/items", "create_item", |_req, _params, _state| async {
                StatusCode::CREATED
            });

        let routes = router.routes();
        assert_eq!(routes[0].method, "POST");
        assert_eq!(routes[0].handler_name, "create_item");
    }

    #[test]
    fn test_router_put_named() {
        let router =
            Router::new().put_named("/items/:id", "update_item", |_req, _params, _state| async {
                StatusCode::OK
            });

        let routes = router.routes();
        assert_eq!(routes[0].method, "PUT");
        assert_eq!(routes[0].handler_name, "update_item");
    }

    #[test]
    fn test_router_delete_named() {
        let router = Router::new().delete_named(
            "/items/:id",
            "delete_item",
            |_req, _params, _state| async { StatusCode::OK },
        );

        let routes = router.routes();
        assert_eq!(routes[0].method, "DELETE");
        assert_eq!(routes[0].handler_name, "delete_item");
    }

    #[test]
    fn test_router_routes_empty() {
        let router = Router::new();
        assert!(router.routes().is_empty());
    }

    #[test]
    fn test_router_routes_mixed_named_and_default() {
        let router = Router::new()
            .get_named("/named", "named_handler", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(Method::GET, "/default", |_req, _params, _state| async {
                StatusCode::OK
            });

        let routes = router.routes();
        assert_eq!(routes[0].handler_name, "named_handler");
        assert_eq!(routes[1].handler_name, "handler");
    }

    #[test]
    fn test_join_group_route_pattern() {
        assert_eq!(
            Router::join_group_route_pattern("/api", "/users"),
            "/api/users"
        );
        assert_eq!(
            Router::join_group_route_pattern("/api/", "/users"),
            "/api/users"
        );
        assert_eq!(
            Router::join_group_route_pattern("/api", "users"),
            "/api/users"
        );
        assert_eq!(
            Router::join_group_route_pattern("/api/", "/users/"),
            "/api/users/"
        );
        assert_eq!(Router::join_group_route_pattern("", "/users"), "/users");
        assert_eq!(Router::join_group_route_pattern("/api", ""), "/api");
    }

    #[test]
    #[should_panic(expected = "A group's prefix pattern must start with /")]
    fn test_invalid_router_group_prefix_pattern() {
        Router::new().group("api/users", Router::new());
    }

    #[test]
    fn test_is_dynamic() {
        assert!(!super::is_dynamic("/health"));
        assert!(!super::is_dynamic("/api/users"));
        assert!(!super::is_dynamic("/api/v1:latest"));
        assert!(super::is_dynamic("/users/:id"));
        assert!(super::is_dynamic("/users/:id/posts/:pid"));
    }

    #[test]
    fn test_route_specificity() {
        assert_eq!(super::route_specificity("/users/current"), vec![0, 0, 0]);
        assert_eq!(super::route_specificity("/users/:id"), vec![0, 0, 1]);
        assert_eq!(
            super::route_specificity("/users/:id/:action"),
            vec![0, 0, 1, 1]
        );
        assert_eq!(
            super::route_specificity("/users/:id/posts"),
            vec![0, 0, 1, 0]
        );
    }

    #[test]
    fn test_sort_routes_static_before_param() {
        let mut router = Router::new()
            .route(Method::GET, "/users/:id", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(
                Method::GET,
                "/users/current",
                |_req, _params, _state| async { StatusCode::OK },
            );

        router.sort_routes();

        assert_eq!(router.routes[0].1.pattern, "/users/current");
        assert_eq!(router.routes[1].1.pattern, "/users/:id");
    }

    #[test]
    fn test_router_group() {
        let users_router = Router::new()
            .get_named("", "list_users", |_req, _params, _state| async {
                StatusCode::OK
            })
            .post_named("", "create_user", |_req, _params, _state| async {
                StatusCode::CREATED
            })
            .get_named("/:id", "get_user", |_req, _params, _state| async {
                StatusCode::OK
            });

        let router = Router::new()
            .get_named("/health", "health_check", |_req, _params, _state| async {
                StatusCode::OK
            })
            .group("/api/users", users_router);

        let routes = router.routes();
        assert_eq!(routes.len(), 4);
        assert_eq!(routes[0].path, "/health");
        assert_eq!(routes[1].path, "/api/users");
        assert_eq!(routes[1].handler_name, "list_users");
        assert_eq!(routes[2].path, "/api/users");
        assert_eq!(routes[2].handler_name, "create_user");
        assert_eq!(routes[3].path, "/api/users/:id");
        assert_eq!(routes[3].handler_name, "get_user");
    }

    #[test]
    fn test_resolve_static_route_after_freeze() {
        let mut router =
            Router::new().route(Method::GET, "/health", |_, _, _| async { StatusCode::OK });
        router.sort_routes();
        router.freeze();

        let result = router.resolve(&Method::GET, "/health");
        assert!(result.is_some());
        let (idx, params) = result.unwrap();
        assert_eq!(idx, 0);
        assert!(params.is_empty());
    }

    #[test]
    fn test_resolve_dynamic_route_extracts_params_after_freeze() {
        let mut router = Router::new().route(Method::GET, "/users/:id", |_, _, _| async {
            StatusCode::OK
        });
        router.sort_routes();
        router.freeze();

        let result = router.resolve(&Method::GET, "/users/42");
        assert!(result.is_some());
        let (idx, params) = result.unwrap();
        assert_eq!(idx, 0);
        assert_eq!(params.get("id").unwrap(), "42");
    }

    #[test]
    fn test_resolve_returns_none_for_unmatched_path() {
        let mut router =
            Router::new().route(Method::GET, "/health", |_, _, _| async { StatusCode::OK });
        router.sort_routes();
        router.freeze();

        assert!(router.resolve(&Method::GET, "/missing").is_none());
    }

    #[test]
    fn test_resolve_returns_none_for_wrong_method() {
        let mut router =
            Router::new().route(Method::GET, "/health", |_, _, _| async { StatusCode::OK });
        router.sort_routes();
        router.freeze();

        assert!(router.resolve(&Method::POST, "/health").is_none());
    }

    #[test]
    fn test_freeze_is_idempotent() {
        let mut router =
            Router::new().route(Method::GET, "/health", |_, _, _| async { StatusCode::OK });
        router.sort_routes();
        router.freeze();
        router.freeze(); // second call must not clear or corrupt state

        assert!(router.resolve(&Method::GET, "/health").is_some());
    }

    #[test]
    fn test_patch_named_sets_method_and_handler_name() {
        let router = Router::new().patch_named("/items/:id", "patch_item", |_, _, _| async {
            StatusCode::OK
        });

        let routes = router.routes();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "PATCH");
        assert_eq!(routes[0].path, "/items/:id");
        assert_eq!(routes[0].handler_name, "patch_item");
    }

    #[test]
    fn test_group_preserves_handler_name_from_sub_router() {
        let inner = Router::new().get_named("/:id", "get_item", |_, _, _| async { StatusCode::OK });
        let router = Router::new().group("/items", inner);

        let routes = router.routes();
        assert_eq!(routes[0].path, "/items/:id");
        assert_eq!(routes[0].handler_name, "get_item");
    }

    #[test]
    fn test_resolve_linear_and_resolve_return_same_index() {
        let mut router = Router::new()
            .route(Method::GET, "/users", |_, _, _| async { StatusCode::OK })
            .route(Method::GET, "/users/:id", |_, _, _| async {
                StatusCode::OK
            });
        router.sort_routes();
        router.freeze();

        let fast_dyn = router.resolve(&Method::GET, "/users/42");
        let slow_dyn = router.resolve_linear(&Method::GET, "/users/42");
        assert_eq!(fast_dyn.map(|(i, _)| i), slow_dyn.map(|(i, _)| i));

        let fast_static = router.resolve(&Method::GET, "/users");
        let slow_static = router.resolve_linear(&Method::GET, "/users");
        assert_eq!(fast_static.map(|(i, _)| i), slow_static.map(|(i, _)| i));
    }

    #[test]
    fn test_multiple_router_groups() {
        let users_router = Router::new()
            .get_named("", "list_users", |_req, _params, _state| async {
                StatusCode::OK
            })
            .post_named("", "create_user", |_req, _params, _state| async {
                StatusCode::CREATED
            })
            .get_named("/:id", "get_user", |_req, _params, _state| async {
                StatusCode::OK
            });

        let invoices_router = Router::new()
            .get_named("", "list_invoices", |_req, _params, _state| async {
                StatusCode::OK
            })
            .get_named("/:id", "get_invoice", |_req, _params, _state| async {
                StatusCode::OK
            });

        let router = Router::new()
            .get_named("/health", "health_check", |_req, _params, _state| async {
                StatusCode::OK
            })
            .group("/api/users", users_router)
            .group("/api/invoices", invoices_router);

        let routes = router.routes();
        assert_eq!(routes.len(), 6);
        assert_eq!(routes[0].path, "/health");
        assert_eq!(routes[1].path, "/api/users");
        assert_eq!(routes[1].handler_name, "list_users");
        assert_eq!(routes[2].path, "/api/users");
        assert_eq!(routes[2].handler_name, "create_user");
        assert_eq!(routes[3].path, "/api/users/:id");
        assert_eq!(routes[3].handler_name, "get_user");
        assert_eq!(routes[4].path, "/api/invoices");
        assert_eq!(routes[4].handler_name, "list_invoices");
        assert_eq!(routes[5].path, "/api/invoices/:id");
        assert_eq!(routes[5].handler_name, "get_invoice");
    }
}
