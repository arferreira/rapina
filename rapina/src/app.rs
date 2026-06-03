//! The main application builder for Rapina.

use crate::auth::{AuthConfig, AuthMiddleware, PublicRoutes};
#[cfg(feature = "cron-scheduler")]
use crate::cron_scheduler::CronScheduler;
use crate::health::{HealthRegistry, health_check, liveness_check, readiness_check};
use crate::introspection::{
    LlmsRegistry, RouteRegistry, list_routes, llms_txt_handler, to_llms_txt,
};
#[cfg(feature = "metrics")]
use crate::metrics::{MetricsMiddleware, MetricsRegistry, metrics_handler};
#[cfg(feature = "compression")]
use crate::middleware::{CompressionConfig, CompressionMiddleware};
use crate::middleware::{
    CorsConfig, CorsMiddleware, Middleware, MiddlewareStack, RequestLogConfig, RequestLogMiddleware,
};
#[cfg(feature = "rate-limit")]
use crate::middleware::{RateLimitConfig, RateLimitMiddleware};
use crate::observability::TracingConfig;
use crate::openapi::{OpenApiRegistry, build_openapi_spec, openapi_spec};
#[cfg(feature = "jwks")]
use crate::prelude::JwksClient;
use crate::router::Router;
use crate::server::{ShutdownHook, serve};
use crate::state::AppState;
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// The main application type for building Rapina servers.
///
/// Use the builder pattern to configure routing, state, middleware,
/// and observability before starting the server.
///
/// # Examples
///
/// ```rust,no_run
/// use rapina::prelude::*;
///
/// #[get("/")]
/// async fn hello() -> &'static str {
///     "Hello!"
/// }
///
/// #[tokio::main]
/// async fn main() -> std::io::Result<()> {
///     let router = Router::new()
///         .get("/", hello);
///
///     Rapina::new()
///         .router(router)
///         .listen("127.0.0.1:3000")
///         .await
/// }
/// ```
pub struct Rapina {
    /// The router for this application.
    pub(crate) router: Router,
    /// The application state.
    pub(crate) state: AppState,
    /// The middleware stack.
    pub(crate) middlewares: MiddlewareStack,
    /// Whether introspection is enabled.
    pub(crate) introspection: bool,
    /// Whether health check endpoint is enabled
    pub(crate) health_check: bool,
    /// Custom health checks
    pub(crate) health_registry: HealthRegistry,
    /// Whether metrics is enabled.
    #[cfg(feature = "metrics")]
    pub(crate) metrics: bool,
    /// Whether llms.txt endpoint is enabled
    pub(crate) llms_txt: bool,
    /// Custom Prometheus collectors to include in the metrics endpoint.
    #[cfg(feature = "metrics")]
    pub(crate) custom_metrics: Vec<Box<dyn prometheus::core::Collector>>,
    /// Whether OpenAPI is enabled
    pub(crate) openapi: bool,
    pub(crate) openapi_title: String,
    pub(crate) openapi_version: String,
    /// Authentication configuration (if enabled)
    pub(crate) auth_config: Option<AuthConfig>,
    /// Public routes registry
    pub(crate) public_routes: PublicRoutes,
    /// Whether auto-discovery is enabled
    pub(crate) auto_discover: bool,
    /// Graceful shutdown timeout (default 30s)
    pub(crate) shutdown_timeout: Duration,
    /// Hooks to run during graceful shutdown
    pub(crate) shutdown_hooks: Vec<ShutdownHook>,
    /// Relay configuration (if enabled)
    #[cfg(feature = "websocket")]
    pub(crate) relay_config: Option<crate::relay::RelayConfig>,
    /// Whether to use RFC 7807 Problem Details for error responses (default: false)
    pub(crate) rfc7807_errors: bool,
    /// Custom base URI for RFC 7807 `type` field (default: "about:blank")
    pub(crate) rfc7807_base_uri: String,
    /// Background job worker configuration. `None` means the worker is disabled.
    #[cfg(feature = "database")]
    pub(crate) jobs_config: Option<crate::jobs::JobConfig>,
    #[cfg(feature = "cron-scheduler")]
    pub(crate) cron_scheduler: Option<CronScheduler>,
    /// Tracing/logging configuration, applied when the server starts.
    pub(crate) tracing_config: Option<TracingConfig>,
    /// OTLP telemetry export configuration (if enabled)
    #[cfg(feature = "otel")]
    pub(crate) telemetry_config: Option<crate::observability::TelemetryConfig>,
}

/// Tracks whether the process-global telemetry pipeline has been installed, so a
/// second `listen` does not build a second exporter or silently fail to install.
#[cfg(feature = "otel")]
static TELEMETRY_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

// Resolves the listen address, preferring RAPINA_HOST/RAPINA_PORT over addr when both are set.
pub(crate) fn resolve_listen_addr(addr: &str) -> SocketAddr {
    match (std::env::var("RAPINA_HOST"), std::env::var("RAPINA_PORT")) {
        (Ok(host), Ok(port)) => format!("{host}:{port}")
            .parse()
            .expect("invalid RAPINA_HOST/RAPINA_PORT"),
        _ => addr.parse().expect("invalid address"),
    }
}

impl Rapina {
    /// Creates a new Rapina application builder.
    ///
    /// Introspection is enabled by default in debug builds.
    pub fn new() -> Self {
        Self {
            router: Router::new(),
            state: AppState::new(),
            middlewares: MiddlewareStack::new(),
            introspection: cfg!(debug_assertions),
            health_check: false,
            health_registry: HealthRegistry::new(),
            #[cfg(feature = "metrics")]
            metrics: false,
            llms_txt: cfg!(debug_assertions),
            #[cfg(feature = "metrics")]
            custom_metrics: Vec::new(),
            openapi: false,
            openapi_title: "API".to_string(),
            openapi_version: "1.0.0".to_string(),
            auth_config: None,
            public_routes: PublicRoutes::new(),
            auto_discover: false,
            shutdown_timeout: Duration::from_secs(30),
            shutdown_hooks: Vec::new(),
            #[cfg(feature = "websocket")]
            relay_config: None,
            rfc7807_errors: false,
            rfc7807_base_uri: "about:blank".to_string(),
            #[cfg(feature = "database")]
            jobs_config: None,
            #[cfg(feature = "cron-scheduler")]
            cron_scheduler: None,
            tracing_config: None,
            #[cfg(feature = "otel")]
            telemetry_config: None,
        }
    }

    /// Sets the router for the application.
    pub fn router(mut self, router: Router) -> Self {
        self.router = router;
        self
    }

    /// Enables route auto-discovery.
    ///
    /// When enabled, handlers annotated with `#[get]`, `#[post]`, `#[put]`,
    /// `#[delete]` are automatically registered at startup via `inventory`.
    /// Routes marked with `#[public]` are automatically added to the public
    /// routes registry (no manual `.public_route()` calls needed).
    ///
    /// Use the `group` parameter on route macros to nest discovered routes
    /// under a prefix: `#[get("/users", group = "/api")]` registers at `/api/users`.
    ///
    /// Discovery is additive with manual `.router()` — both work together.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use rapina::prelude::*;
    ///
    /// #[get("/")]
    /// async fn hello() -> &'static str { "Hello!" }
    ///
    /// #[get("/users", group = "/api")]
    /// async fn list_users() -> &'static str { "users" }
    ///
    /// #[tokio::main]
    /// async fn main() -> std::io::Result<()> {
    ///     Rapina::new()
    ///         .discover()
    ///         .listen("127.0.0.1:3000")
    ///         .await
    /// }
    /// ```
    pub fn discover(mut self) -> Self {
        self.auto_discover = true;
        self
    }

    /// Adds shared state that can be accessed by handlers via [`State`](crate::extract::State).
    pub fn state<T: Send + Sync + 'static>(mut self, value: T) -> Self {
        self.state = self.state.with(value);
        self
    }

    /// Adds a pre-existing `Arc<T>` as shared state.
    ///
    /// This is the preferred way to register trait objects for dependency
    /// injection.  Without this method, passing an `Arc<dyn MyTrait>` to
    /// [`.state()`](Self::state) requires a newtype wrapper; with `state_arc`
    /// you can register the arc directly.
    ///
    /// With `state_arc` the `Arc` is registered under its own [`TypeId`], so
    /// no newtype wrapper is required.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use rapina::prelude::*;
    /// use std::sync::Arc;
    ///
    /// trait MyRepo: Send + Sync {
    ///     fn find_all(&self) -> Vec<String>;
    /// }
    ///
    /// struct PgRepo;
    /// impl MyRepo for PgRepo {
    ///     fn find_all(&self) -> Vec<String> { vec![] }
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() -> std::io::Result<()> {
    ///     Rapina::new()
    ///         .state_arc(Arc::new(PgRepo) as Arc<dyn MyRepo>)
    ///         .listen("127.0.0.1:3000")
    ///         .await
    /// }
    /// ```
    pub fn state_arc<T: ?Sized + Send + Sync + 'static>(
        mut self,
        value: std::sync::Arc<T>,
    ) -> Self {
        self.state = self.state.with_arc(value);
        self
    }

    /// Adds a middleware to the application.
    pub fn middleware<M: Middleware>(mut self, middleware: M) -> Self {
        self.middlewares.add(middleware);
        self
    }

    /// Adds a tower layer as middleware.
    ///
    /// This is a convenience method equivalent to
    /// `.middleware(TowerLayerMiddleware::new(layer))`.
    ///
    /// Requires the `tower` feature.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rapina::prelude::*;
    ///
    /// Rapina::new()
    ///     .layer(my_tower_layer)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    #[cfg(feature = "tower")]
    pub fn layer<L>(self, layer: L) -> Self
    where
        L: tower_layer::Layer<crate::middleware::NextService>,
        crate::middleware::TowerLayerMiddleware<L>: Middleware,
    {
        self.middleware(crate::middleware::TowerLayerMiddleware::new(layer))
    }

    /// Enables CORS for the application.
    ///
    /// Use `CorsConfig::permisive()` for development (it allows all origins),
    /// or `CorsConfig::with_origins()` for production with specific origins.
    ///
    /// # Example
    ///
    /// ```ignore
    ///  Rapina::new()
    ///  .with_cors(CorsConfig::permisive())
    ///  .router(router)
    ///  .listen("127.0.0.1:3000")
    ///  .await
    pub fn with_cors(mut self, config: CorsConfig) -> Self {
        self.middlewares.add(CorsMiddleware::new(config));
        self
    }

    /// Enables rate limiting for the application.
    ///
    /// Uses a token bucket algorithm to limit requests per client.
    /// By default, clients are identified by IP address.
    ///
    /// # Example
    ///
    /// ```ignore
    /// Rapina::new()
    ///     .with_rate_limit(RateLimitConfig::per_minute(100))
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    #[cfg(feature = "rate-limit")]
    pub fn with_rate_limit(mut self, config: RateLimitConfig) -> Self {
        self.middlewares.add(RateLimitMiddleware::new(config));
        self
    }

    /// Enables configurable request/response logging.
    ///
    /// Use `RequestLogConfig::verbose()` for full logging with default
    /// redaction, or build a custom config to pick individual fields.
    ///
    /// # Example
    ///
    /// ```ignore
    /// Rapina::new()
    ///     .with_request_log(RequestLogConfig::verbose())
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    pub fn with_request_log(mut self, config: RequestLogConfig) -> Self {
        self.middlewares
            .add(RequestLogMiddleware::with_config(config));
        self
    }

    /// Enables response compression (gzip, deflate).
    #[cfg(feature = "compression")]
    pub fn with_compression(mut self, config: CompressionConfig) -> Self {
        self.middlewares.add(CompressionMiddleware::new(config));
        self
    }

    /// Enables the Relay system for real-time push over WebSocket.
    ///
    /// Registers a WebSocket endpoint (default `/ws`) through the normal
    /// routing and middleware stack. If authentication is enabled, mark the
    /// relay path as public with `.public_route("GET", "/ws")` — or leave it
    /// protected so only authenticated clients can connect.
    ///
    /// Handlers use the [`Relay`](crate::relay::Relay) extractor to push
    /// messages to subscribed clients.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rapina::relay::RelayConfig;
    ///
    /// Rapina::new()
    ///     .with_relay(RelayConfig::default())
    ///     .discover()
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    #[cfg(feature = "websocket")]
    pub fn with_relay(mut self, config: crate::relay::RelayConfig) -> Self {
        self.relay_config = Some(config);
        self
    }

    /// Enables JWT authentication with the given configuration.
    ///
    /// When enabled, all routes require a valid `Authorization: Bearer <token>` header
    /// unless marked with `#[public]` or registered via [`public_route`](Self::public_route).
    ///
    /// # Example
    ///
    /// ```ignore
    /// let auth_config = AuthConfig::from_env().expect("JWT_SECRET required");
    ///
    /// Rapina::new()
    ///     .with_auth(auth_config)
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    pub fn with_auth(mut self, config: AuthConfig) -> Self {
        self.auth_config = Some(config);
        self
    }

    /// Registers a route as public (no authentication required).
    ///
    /// Use this for routes that should be accessible without a JWT token.
    /// Routes starting with `/__rapina` are automatically public.
    ///
    /// # Example
    ///
    /// ```ignore
    /// Rapina::new()
    ///     .with_auth(auth_config)
    ///     .public_route("POST", "/login")
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    pub fn public_route(mut self, method: &str, path: &str) -> Self {
        self.public_routes.add(method, path);
        self
    }

    /// Enables the in-process background job worker with the given configuration.
    ///
    /// The worker spawns alongside the HTTP server during [`listen`](Self::listen)
    /// and polls `rapina_jobs` on the configured interval. It shuts down
    /// gracefully when the server receives SIGINT or SIGTERM, finishing the
    /// current job batch before exiting.
    ///
    /// Requires [`with_database`](Self::with_database) to be called first —
    /// the worker uses the same `DatabaseConnection` stored in `AppState`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use rapina::jobs::JobConfig;
    /// use std::time::Duration;
    ///
    /// Rapina::new()
    ///     .with_database(db_config).await?
    ///     .jobs(JobConfig::default().queues(["default", "emails"]))
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    #[cfg(feature = "database")]
    pub fn jobs(mut self, config: crate::jobs::JobConfig) -> Self {
        self.jobs_config = Some(config);
        self
    }

    /// Configures tracing/logging for the application.
    ///
    /// The subscriber is installed when the server starts (in [`listen`]), so it
    /// can be composed with the OTLP export layer from [`with_telemetry`] onto a
    /// single global subscriber.
    ///
    /// [`listen`]: Self::listen
    /// [`with_telemetry`]: Self::with_telemetry
    pub fn with_tracing(mut self, config: TracingConfig) -> Self {
        self.tracing_config = Some(config);
        self
    }

    /// Configures OTLP trace export to a collector such as Jaeger or Datadog.
    ///
    /// Traces are exported over OTLP gRPC and incoming W3C `traceparent` headers
    /// are honored so traces continue across service boundaries. The exporter is
    /// built and the subscriber installed when the server starts (in [`listen`]).
    ///
    /// [`listen`]: Self::listen
    #[cfg(feature = "otel")]
    pub fn with_telemetry(mut self, config: crate::observability::TelemetryConfig) -> Self {
        self.telemetry_config = Some(config);
        self
    }

    /// Enables or disables the introspection endpoint.
    ///
    /// When enabled, a `GET /__rapina/routes` endpoint is registered
    /// that returns all routes as JSON.
    ///
    /// Introspection is enabled by default in debug builds.
    /// Use this when the value is dynamic (e.g. read from config/env).
    /// For literal call sites, prefer [`enable_introspection`](Self::enable_introspection)
    /// or [`disable_introspection`](Self::disable_introspection).
    pub fn with_introspection(mut self, enabled: bool) -> Self {
        self.introspection = enabled;
        self
    }

    /// Enables the introspection endpoint.
    ///
    /// Convenience wrapper for `.with_introspection(true)`.
    pub fn enable_introspection(self) -> Self {
        self.with_introspection(true)
    }

    /// Disables the introspection endpoint.
    ///
    /// Convenience wrapper for `.with_introspection(false)`.
    /// Useful in tests or production builds where introspection should be
    /// suppressed even when debug assertions are active.
    pub fn disable_introspection(self) -> Self {
        self.with_introspection(false)
    }

    /// Enables or disables the `/__rapina/llms.txt` endpoint.
    ///
    /// When enabled, a Markdown document following the llms.txt convention is
    /// generated once at startup from the registered routes and served at
    /// `GET /__rapina/llms.txt` as `text/plain; charset=utf-8`.
    ///
    /// Defaults to `true` in debug builds, `false` in release builds.
    pub fn with_llms_txt(mut self, enabled: bool) -> Self {
        self.llms_txt = enabled;
        self
    }

    /// Enables the `/__rapina/llms.txt` endpoint.
    pub fn enable_llms_txt(self) -> Self {
        self.with_llms_txt(true)
    }

    /// Disables the `/__rapina/llms.txt` endpoint.
    pub fn disable_llms_txt(self) -> Self {
        self.with_llms_txt(false)
    }

    /// Health check is disabled by default.
    /// Enables or disables the built-in health check endpoints.
    ///
    /// When enabled, registers readiness and liveness health check endpoints:
    /// - `GET /__rapina/health` — alias for readiness, for simple setups and load balancers
    /// - `GET /__rapina/health/live` — liveness probe, always returns `200 OK`
    /// - `GET /__rapina/health/ready` — readiness probe, runs DB and custom checks
    ///
    /// Prefer this when the value is dynamic (e.g. read from config/env):
    ///
    /// ```ignore
    /// let cfg = Config::from_env();
    /// Rapina::new().with_health_check(cfg.health_check_enabled);
    /// ```
    ///
    /// For fixed call sites, [`enable_health_check`](Self::enable_health_check) reads more clearly:
    ///
    /// ```rust,no_run
    /// # use rapina::prelude::*;
    /// Rapina::new().enable_health_check();
    /// ```
    pub fn with_health_check(mut self, enabled: bool) -> Self {
        self.health_check = enabled;
        self
    }

    /// Enables the built-in health check endpoints.
    ///
    /// Convenience wrapper for `.with_health_check(true)`. See
    /// [`with_health_check`](Self::with_health_check) for the dynamic-config form.
    pub fn enable_health_check(self) -> Self {
        self.with_health_check(true)
    }

    /// Disables the built-in health check endpoints.
    ///
    /// Convenience wrapper for `.with_health_check(false)`. See
    /// [`with_health_check`](Self::with_health_check) for the dynamic-config form.
    pub fn disable_health_check(self) -> Self {
        self.with_health_check(false)
    }

    /// Registers a new scheduled cronjob.
    /// The `CronScheduler` instance will be started during Rapina's webserver startup phase.
    #[cfg(feature = "cron-scheduler")]
    pub fn cron<F, Fut, E>(mut self, cron_schedule: &str, task: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<(), E>> + Send + 'static,
        E: std::error::Error + Send + 'static,
    {
        // Lazily initialize the scheduler if it's still None and get a mutable reference
        let cron_scheduler = self.cron_scheduler.get_or_insert_with(CronScheduler::new);
        cron_scheduler
            .schedule(cron_schedule.to_string(), task)
            .expect("Failed to schedule cron job");

        self
    }

    /// Starts the Cronjob Scheduler and registers the shutdown hook
    #[cfg(feature = "cron-scheduler")]
    async fn start_cronjob_scheduler(mut self) -> Self {
        if let Some(mut cron_scheduler) = self.cron_scheduler.take() {
            cron_scheduler.start().await;
            self = self.on_shutdown(move || async move {
                cron_scheduler.shutdown().await;
            });
        }
        self
    }

    /// Warms up the JWKS cache immediately without waiting for the next cronjob tick
    #[cfg(feature = "jwks")]
    async fn warmup_jwks_cache(&self) -> () {
        let Some(jwks_client) = self.state.get::<JwksClient>().cloned() else {
            tracing::error!(
                "Skipped warmup of JWKS cache because the Rapina state for JwksClient is empty. Did you forget to call .state(jwks_client)?"
            );

            return;
        };

        match jwks_client.refresh_jwks_cache().await {
            Ok(_) => tracing::info!("Successfully warmed up JWKS cache"),
            Err(e) => tracing::error!("Failed warmup of JWKS cache: {}", e),
        }
    }

    /// Schedules the JWKS refresh cronjob
    #[cfg(feature = "jwks")]
    fn schedule_jwks_cronjob(mut self) -> Self {
        let Some(jwks_client) = self.state.get::<JwksClient>().cloned() else {
            tracing::error!(
                "Skipped scheduling the JWKS refresh cronjob because the Rapina state for JwksClient is empty. Did you forget to call .state(jwks_client)?"
            );
            return self;
        };

        let refresh_schedule = jwks_client.refresh_schedule().to_owned();
        let jwks_client = jwks_client.clone();

        self = self.cron(&refresh_schedule, move || {
            let jwks_client = jwks_client.clone();
            async move { jwks_client.refresh_jwks_cache().await }
        });

        tracing::info!(
            "Scheduled JWKS refresh cronjob with schedule '{}'",
            refresh_schedule
        );
        self
    }

    /// The function is called on every `GET /__rapina/health` request.
    /// Registers a custom health check function.
    ///
    /// Registers a named async health check function.
    ///
    /// The function is called on every `GET /__rapina/health/ready` request.
    /// Return `true` if the dependency is healthy, `false` otherwise.
    ///
    /// Note: this registers a check function — it does **not** enable the health
    /// endpoints. You still need to call `.enable_health_check()` (or
    /// `.with_health_check(true)` for dynamic config) to activate the endpoints.
    /// A warning is emitted at startup if checks are registered but the endpoint
    /// is disabled.
    pub fn add_health_check<F, Fut>(mut self, name: &'static str, f: F) -> Self
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: std::future::Future<Output = bool> + Send + 'static,
    {
        self.health_registry.add(name, f);
        self
    }

    /// Enables RFC 7807 Problem Details for error responses.
    ///
    /// When enabled, error responses use the `application/problem+json`
    /// content type and follow the RFC 7807 structure.
    ///
    /// This is disabled by default for backwards compatibility.
    pub fn enable_rfc7807_errors(mut self) -> Self {
        self.rfc7807_errors = true;
        self
    }

    /// Sets the base URI used for RFC 7807 `type` field.
    ///
    /// Error codes are appended as kebab-case path segments.
    /// For example, with `"https://myapp.com/errors"`, a `NOT_FOUND`
    /// error produces `"https://myapp.com/errors/not-found"`.
    ///
    /// Defaults to `"about:blank"` per RFC 7807 §4.2.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let app = Rapina::new()
    ///     .enable_rfc7807_errors()
    ///     .rfc7807_base_uri("https://myapp.com/errors");
    /// ```
    pub fn rfc7807_base_uri(mut self, uri: impl Into<String>) -> Self {
        self.rfc7807_base_uri = uri.into();
        self
    }

    /// Sets the graceful shutdown timeout.
    ///
    /// When the server receives a shutdown signal (SIGINT/SIGTERM), it stops
    /// accepting new connections and waits up to this duration for in-flight
    /// requests to complete. Defaults to 30 seconds.
    pub fn shutdown_timeout(mut self, timeout: Duration) -> Self {
        self.shutdown_timeout = timeout;
        self
    }

    /// Registers an async hook to run during graceful shutdown.
    ///
    /// Hooks run after in-flight connections have drained (or the timeout
    /// expires), in the order they were registered. Use this to close
    /// database pools, flush metrics, or perform other cleanup.
    ///
    /// # Example
    ///
    /// ```ignore
    /// Rapina::new()
    ///     .on_shutdown(|| async {
    ///         println!("cleaning up...");
    ///     })
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    pub fn on_shutdown<F, Fut>(mut self, hook: F) -> Self
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.shutdown_hooks.push(Box::new(move || Box::pin(hook())));
        self
    }

    /// Metrics is disabled by default unless you call `with_metrics(true)`.
    /// Enables or disables the metrics endpoint.
    ///
    /// When enabled, a `GET /metrics` endpoint is registered
    /// that returns all metrics to Prometheus.
    ///
    /// Use this when the value is dynamic (e.g. read from config/env).
    /// For literal call sites, prefer [`enable_metrics`](Self::enable_metrics)
    /// or [`disable_metrics`](Self::disable_metrics).
    #[cfg(feature = "metrics")]
    pub fn with_metrics(mut self, enabled: bool) -> Self {
        self.metrics = enabled;
        self
    }

    /// Enables the metrics endpoint.
    ///
    /// Convenience wrapper for `.with_metrics(true)`.
    #[cfg(feature = "metrics")]
    pub fn enable_metrics(self) -> Self {
        self.with_metrics(true)
    }

    /// Disables the metrics endpoint.
    ///
    /// Convenience wrapper for `.with_metrics(false)`.
    #[cfg(feature = "metrics")]
    pub fn disable_metrics(self) -> Self {
        self.with_metrics(false)
    }

    /// Registers a custom Prometheus collector to be included in the `/metrics` endpoint.
    ///
    /// Use this to mix your own application metrics — counters, gauges, histograms — into
    /// the same endpoint that Rapina uses for its built-in HTTP metrics.
    ///
    /// Requires the `metrics` feature and `.enable_metrics()` (or `.with_metrics(true)`)
    /// to be called as well.
    ///
    /// # Panics
    ///
    /// Panics at startup if the collector's metric name collides with a built-in Rapina metric
    /// (`http_requests_total`, `http_request_duration_seconds`, `http_requests_in_flight`) or with
    /// another previously registered custom collector. Choose unique metric names to avoid this.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use rapina::prelude::*;
    /// use prometheus::{IntCounterVec, Opts};
    ///
    /// let orders_total = IntCounterVec::new(
    ///     Opts::new("orders_total", "Total number of orders placed"),
    ///     &["status"],
    /// )
    /// .unwrap();
    ///
    /// Rapina::new()
    ///     .enable_metrics()
    ///     .add_metric(orders_total.clone())
    ///     .listen("127.0.0.1:3000");
    /// ```
    #[cfg(feature = "metrics")]
    pub fn add_metric(mut self, collector: impl prometheus::core::Collector + 'static) -> Self {
        self.custom_metrics.push(Box::new(collector));
        self
    }

    /// Enables or disables openapi endpoint
    ///
    /// When enabled, a get `/__rapina/openapi.json` endpoint is registered
    /// that returns all routes as OpenAPI specification
    /// OpenAPI is disabled by default
    pub fn openapi(mut self, title: impl Into<String>, version: impl Into<String>) -> Self {
        self.openapi = true;
        self.openapi_title = title.into();
        self.openapi_version = version.into();
        self
    }

    /// Enables response caching with the given configuration.
    ///
    /// Caches GET responses that use `#[cache(ttl = N)]` and auto-invalidates
    /// on successful mutations (POST/PUT/DELETE/PATCH).
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rapina::cache::CacheConfig;
    ///
    /// Rapina::new()
    ///     .with_cache(CacheConfig::in_memory(1000)).await?
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    pub async fn with_cache(
        mut self,
        config: crate::cache::CacheConfig,
    ) -> Result<Self, std::io::Error> {
        let backend = config.build().await?;
        self.middlewares
            .add(crate::cache::CacheMiddleware::new(backend));
        Ok(self)
    }

    /// Configures database connection with the given configuration.
    ///
    /// This method connects to the database and registers the connection
    /// in the application state. Use the [`Db`](crate::database::Db) extractor
    /// in your handlers to access the connection.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rapina::prelude::*;
    /// use rapina::database::{DatabaseConfig, Db};
    ///
    /// #[get("/users")]
    /// async fn list_users(db: Db) -> Result<Json<Vec<User>>> {
    ///     let users = UserEntity::find().all(db.conn()).await?;
    ///     Ok(Json(users))
    /// }
    ///
    /// #[tokio::main]
    /// async fn main() -> std::io::Result<()> {
    ///     let db_config = DatabaseConfig::from_env()?;
    ///
    ///     Rapina::new()
    ///         .with_database(db_config).await?
    ///         .router(router)
    ///         .listen("127.0.0.1:3000")
    ///         .await
    /// }
    /// ```
    #[cfg(feature = "database")]
    pub async fn with_database(
        mut self,
        config: crate::database::DatabaseConfig,
    ) -> Result<Self, std::io::Error> {
        let policy = crate::database::DatabaseMigrationPolicy {
            auto_apply: config.auto_migrate,
        };
        let conn = config
            .connect()
            .await
            .map_err(|e| std::io::Error::other(format!("Database connection failed: {}", e)))?;
        self.state = self.state.with(conn).with(policy);
        Ok(self)
    }

    /// Runs database migrations according to [`DatabaseConfig::auto_migrate`](crate::database::DatabaseConfig::auto_migrate), or only warns about pending migrations when it is `false`.
    ///
    /// Call this after `with_database()` so the policy from configuration is available. If only a raw [`DatabaseConnection`] was registered (for example via [`Rapina::state`](crate::app::Rapina::state)), pending migrations are **not** applied and a warning is logged if any are pending (same as `auto_migrate: false`).
    ///
    /// # Example
    ///
    /// ```ignore
    /// mod migrations;
    ///
    /// Rapina::new()
    ///     .with_database(
    ///         DatabaseConfig::new("sqlite://app.db?mode=rwc").auto_migrate(true),
    ///     ).await?
    ///     .run_migrations::<migrations::Migrator>().await?
    ///     .router(router)
    ///     .listen("127.0.0.1:3000")
    ///     .await
    /// ```
    #[cfg(feature = "database")]
    pub async fn run_migrations<M: crate::migration::MigratorTrait>(
        self,
    ) -> Result<Self, std::io::Error> {
        let conn = self
            .state
            .get::<sea_orm::DatabaseConnection>()
            .ok_or_else(|| {
                std::io::Error::other(
                    "Database connection not found in application state. Register it with .with_database() or .state(DatabaseConnection).",
                )
            })?;

        let auto_apply = self
            .state
            .get::<crate::database::DatabaseMigrationPolicy>()
            .map(|p| p.auto_apply)
            .unwrap_or(false);

        crate::migration::run_startup_migrations::<M>(conn, auto_apply)
            .await
            .map_err(|e| std::io::Error::other(format!("Migration failed: {}", e)))?;

        Ok(self)
    }

    /// Applies all deferred setup (auth middleware, introspection, metrics, openapi).
    ///
    /// Both [`listen`](Self::listen) and [`TestClient::new`](crate::testing::TestClient::new)
    /// call this so the app behaves identically in tests and production.
    pub(crate) fn prepare(mut self) -> Self {
        self.state = self.state.with(crate::error::ErrorConfig {
            use_rfc7807: self.rfc7807_errors,
            base_uri: self.rfc7807_base_uri.clone(),
        });

        // Trace context propagation runs first so the request span carries the
        // parent extracted from incoming headers and wraps every other layer.
        #[cfg(feature = "otel")]
        if self.telemetry_config.is_some() {
            self.middlewares
                .prepend(crate::middleware::TraceContextMiddleware::new());
        }

        // Auto-discover routes from inventory (must run before auth middleware)
        if self.auto_discover {
            let manual_count = self.router.routes.len();

            let public_names: std::collections::HashSet<&str> =
                inventory::iter::<crate::discovery::PublicMarker>
                    .into_iter()
                    .map(|m| m.handler_name)
                    .collect();

            let mut discovered_public = 0usize;
            for descriptor in inventory::iter::<crate::discovery::RouteDescriptor> {
                self.router = (descriptor.register)(self.router);
                if descriptor.is_public || public_names.contains(descriptor.handler_name) {
                    self.public_routes.add(descriptor.method, descriptor.path);
                    discovered_public += 1;
                }
            }

            let discovered_count = self.router.routes.len() - manual_count;
            tracing::info!(
                "Discovered {} routes ({} public)",
                discovered_count,
                discovered_public
            );
        }

        // Register relay WebSocket endpoint as a normal route (goes through middleware stack)
        #[cfg(feature = "websocket")]
        if let Some(config) = self.relay_config.take() {
            let path = config.path.clone();
            let backend = Box::new(crate::relay::InMemoryBackend::new(config.topic_capacity));

            // Collect channel handlers from inventory, sorted by specificity:
            // exact matches first, then prefix matches ordered by match_prefix
            // length descending (longer = more specific). Ensures "room:vip:*"
            // always wins over "room:*" regardless of inventory iteration order.
            let mut channels: Vec<&'static crate::relay::ChannelDescriptor> =
                inventory::iter::<crate::relay::ChannelDescriptor>
                    .into_iter()
                    .collect();
            channels.sort_by(|a, b| match (a.is_prefix, b.is_prefix) {
                (false, true) => std::cmp::Ordering::Less,
                (true, false) => std::cmp::Ordering::Greater,
                (false, false) => a.pattern.cmp(b.pattern),
                (true, true) => b.match_prefix.len().cmp(&a.match_prefix.len()),
            });

            let hub = std::sync::Arc::new(crate::relay::RelayHub::new(config, backend, channels));
            self.state = self.state.with(hub);
            self.router =
                self.router
                    .get_named(&path, "relay_ws", crate::relay::RelayHub::ws_handler);
        }

        // Add auth middleware if configured
        if let Some(auth_config) = self.auth_config.take() {
            let auth_middleware =
                AuthMiddleware::with_public_routes(auth_config, self.public_routes.clone());
            self.middlewares.add(auth_middleware);
        }

        if self.introspection {
            let routes = self.router.routes();
            self.state = self.state.with(RouteRegistry::with_routes(routes));
            self.router = self
                .router
                .get_named("/__rapina/routes", "list_routes", list_routes);
        }

        if self.llms_txt {
            // Snapshot routes BEFORE registering /__rapina/llms.txt so the document
            // doesn't reference itself. The /__rapina/* filter in to_llms_txt is
            // defense-in-depth.
            let routes = self.router.routes();
            let content = to_llms_txt(&self.openapi_title, &routes);
            self.state = self.state.with(LlmsRegistry::new(content));
            self.router = self
                .router
                .get_named("/__rapina/llms.txt", "llms_txt", llms_txt_handler);
        }

        if !self.health_check && !self.health_registry.checks.is_empty() {
            tracing::warn!(
                "add_health_check() was called but the health check endpoint is disabled. \
                Call .enable_health_check() or .with_health_check(true) to activate it."
            );
        }

        if self.health_check {
            let registry = std::mem::take(&mut self.health_registry);
            self.state = self.state.with(registry);
            self.router = self
                .router
                .get_named("/__rapina/health", "health_check", health_check)
                .get_named("/__rapina/health/live", "liveness_check", liveness_check)
                .get_named("/__rapina/health/ready", "readiness_check", readiness_check);
        }

        #[cfg(feature = "metrics")]
        if self.metrics {
            let registry =
                MetricsRegistry::new_with_collectors(std::mem::take(&mut self.custom_metrics));
            self.state = self.state.with(registry.clone());
            self.middlewares.add(MetricsMiddleware::new(registry));
            self.router = self
                .router
                .get_named("/metrics", "metrics", metrics_handler);
        }

        if self.openapi {
            let routes = self.router.routes();
            let spec = build_openapi_spec(&self.openapi_title, &self.openapi_version, &routes);
            self.state = self.state.with(OpenApiRegistry::new(spec));
            self.router =
                self.router
                    .get_named("/__rapina/openapi.json", "openapi_spec", openapi_spec);
        }

        // Sort routes so static segments take priority over parameterized ones.
        // This prevents `/users/:id` from shadowing `/users/current`.
        self.router.sort_routes();

        // Build the static route map for O(1) lookup of parameterless routes.
        self.router.freeze();

        self
    }

    /// Starts the HTTP server on the given address.
    ///
    /// # Panics
    ///
    /// Panics if the address is invalid.
    ///
    /// When `RAPINA_HOST`/`RAPINA_PORT` env vars are set (e.g. under `rapina dev`),
    /// they take priority over the `addr` argument so the CLI is always authoritative
    /// in dev mode and the banner is correct by construction.
    pub async fn listen(self, addr: &str) -> std::io::Result<()> {
        let addr: SocketAddr = resolve_listen_addr(addr);

        // Install the tracing subscriber before anything else so startup logs are
        // captured. When telemetry is configured the OTLP export layer is composed
        // onto the same subscriber, since a global subscriber can only be set once.
        // The global state (subscriber, tracer provider, propagator) is installed
        // at most once per process; a second `listen` reuses it.
        let tracing_config = self.tracing_config.clone();
        #[cfg(feature = "otel")]
        let otel_provider = match self.telemetry_config.clone() {
            Some(config)
                if !TELEMETRY_INITIALIZED.swap(true, std::sync::atomic::Ordering::SeqCst) =>
            {
                let (provider, layer) = crate::observability::build_otel_pipeline(&config)
                    .map_err(std::io::Error::other)?;
                opentelemetry::global::set_tracer_provider(provider.clone());
                opentelemetry::global::set_text_map_propagator(
                    opentelemetry_sdk::propagation::TraceContextPropagator::new(),
                );
                if crate::observability::init_subscriber(tracing_config, Some(layer)).is_err() {
                    eprintln!(
                        "rapina: telemetry is enabled but a tracing subscriber is already installed; \
                         OTLP traces will not be exported"
                    );
                }
                Some(provider)
            }
            Some(_) => {
                eprintln!("rapina: telemetry already initialized; ignoring repeated configuration");
                None
            }
            None => {
                let _ = crate::observability::init_subscriber(tracing_config, None);
                None
            }
        };
        #[cfg(not(feature = "otel"))]
        let _ = crate::observability::init_subscriber(tracing_config, None);

        let app = self.prepare();

        // Flush exported spans during graceful shutdown so in-flight traces are
        // not dropped. The hook runs after connections drain.
        #[cfg(feature = "otel")]
        let app = {
            let mut app = app;
            if let Some(provider) = otel_provider {
                app.shutdown_hooks.push(Box::new(move || {
                    Box::pin(async move {
                        // shutdown joins the exporter's background thread, so run it
                        // off the async runtime worker.
                        let _ = tokio::task::spawn_blocking(move || provider.shutdown()).await;
                    })
                }));
            }
            app
        };

        // Spawn the background job worker if configured.  The worker receives
        // a cheap clone of AppState (inner values are Arc-wrapped) so it shares
        // the same database pool and application state as the HTTP handlers.
        #[cfg(feature = "database")]
        if let Some(config) = app.jobs_config.clone() {
            let state = std::sync::Arc::new(app.state.clone());
            let worker = crate::jobs::worker::Worker::new(state, config);
            tokio::spawn(worker.run());
        }

        #[cfg(feature = "jwks")]
        // Warms up the JWKS cache so it is available immediately when the webserver starts up
        // and registers the JWKS refresh cronjob
        let app = {
            app.warmup_jwks_cache().await;
            app.schedule_jwks_cronjob()
        };

        #[cfg(feature = "cron-scheduler")]
        let app = app.start_cronjob_scheduler().await;

        serve(
            app.router,
            app.state,
            app.middlewares,
            addr,
            app.shutdown_timeout,
            app.shutdown_hooks,
        )
        .await
    }
}

impl Default for Rapina {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::middleware::TimeoutMiddleware;
    #[cfg(feature = "cron-scheduler")]
    use crate::prelude::Error;
    use http::StatusCode;
    use std::time::Duration;

    #[test]
    fn test_rapina_new() {
        let app = Rapina::new();
        assert!(app.middlewares.is_empty());
    }

    #[test]
    fn test_rapina_default() {
        let app = Rapina::default();
        assert!(app.middlewares.is_empty());
    }

    #[test]
    #[serial_test::serial]
    fn test_listen_addr_no_env_vars() {
        unsafe {
            std::env::remove_var("RAPINA_HOST");
            std::env::remove_var("RAPINA_PORT");
        }
        let addr = resolve_listen_addr("127.0.0.1:8000");
        assert_eq!(addr.to_string(), "127.0.0.1:8000");
    }

    #[test]
    #[serial_test::serial]
    fn test_listen_addr_env_vars_override() {
        unsafe {
            std::env::set_var("RAPINA_HOST", "127.0.0.1");
            std::env::set_var("RAPINA_PORT", "9000");
        }
        let addr = resolve_listen_addr("127.0.0.1:8000");
        assert_eq!(addr.to_string(), "127.0.0.1:9000");
        unsafe {
            std::env::remove_var("RAPINA_HOST");
            std::env::remove_var("RAPINA_PORT");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_listen_addr_env_vars_different_host() {
        unsafe {
            std::env::set_var("RAPINA_HOST", "0.0.0.0");
            std::env::set_var("RAPINA_PORT", "3000");
        }
        let addr = resolve_listen_addr("127.0.0.1:3000");
        assert_eq!(addr.to_string(), "0.0.0.0:3000");
        unsafe {
            std::env::remove_var("RAPINA_HOST");
            std::env::remove_var("RAPINA_PORT");
        }
    }

    #[test]
    #[serial_test::serial]
    fn test_listen_addr_partial_env_vars_port_fallback() {
        unsafe {
            std::env::set_var("RAPINA_HOST", "127.0.0.1");
            std::env::remove_var("RAPINA_PORT");
        }
        // Only the host var set — port var falls back to the addr argument
        let addr = resolve_listen_addr("127.0.0.1:8000");
        assert_eq!(addr.to_string(), "127.0.0.1:8000");
        unsafe {
            std::env::remove_var("RAPINA_HOST");
        }
    }
    #[test]
    #[serial_test::serial]
    fn test_listen_addr_partial_env_vars_host_fallback() {
        unsafe {
            std::env::remove_var("RAPINA_HOST");
            std::env::set_var("RAPINA_PORT", "8000");
        }
        // Only the port var set — host var falls back to the addr argument
        let addr = resolve_listen_addr("127.0.0.1:8000");
        assert_eq!(addr.to_string(), "127.0.0.1:8000");
        unsafe {
            std::env::remove_var("RAPINA_PORT");
        }
    }

    #[test]
    fn test_rapina_with_router() {
        let router = Router::new().route(
            http::Method::GET,
            "/health",
            |_req, _params, _state| async { StatusCode::OK },
        );

        let app = Rapina::new().router(router);
        assert!(!app.router.routes.is_empty());
    }

    #[test]
    fn test_rapina_with_state() {
        #[derive(Clone)]
        struct Config {
            name: String,
        }

        let app = Rapina::new().state(Config {
            name: "test".to_string(),
        });

        let config = app.state.get::<Config>().unwrap();
        assert_eq!(config.name, "test");
    }

    #[test]
    fn test_rapina_with_middleware() {
        let app = Rapina::new().middleware(TimeoutMiddleware::new(Duration::from_secs(10)));

        assert!(!app.middlewares.is_empty());
    }

    #[test]
    fn test_rapina_chaining() {
        #[allow(dead_code)]
        #[derive(Clone)]
        struct Config {
            name: String,
        }

        let router = Router::new()
            .route(http::Method::GET, "/", |_req, _params, _state| async {
                StatusCode::OK
            })
            .route(
                http::Method::POST,
                "/users",
                |_req, _params, _state| async { StatusCode::CREATED },
            );

        let app = Rapina::new()
            .router(router)
            .state(Config {
                name: "app".to_string(),
            })
            .middleware(TimeoutMiddleware::default());

        assert!(!app.router.routes.is_empty());
        assert!(app.state.get::<Config>().is_some());
        assert!(!app.middlewares.is_empty());
    }

    #[test]
    fn test_rapina_multiple_states() {
        #[allow(dead_code)]
        #[derive(Clone)]
        struct Config {
            name: String,
        }

        #[allow(dead_code)]
        #[derive(Clone)]
        struct DbPool {
            url: String,
        }

        let app = Rapina::new()
            .state(Config {
                name: "app".to_string(),
            })
            .state(DbPool {
                url: "postgres://localhost".to_string(),
            });

        assert!(app.state.get::<Config>().is_some());
        assert!(app.state.get::<DbPool>().is_some());
    }

    #[test]
    fn test_rapina_multiple_middlewares() {
        use crate::middleware::{BodyLimitMiddleware, TraceIdMiddleware};

        let app = Rapina::new()
            .middleware(TraceIdMiddleware::new())
            .middleware(TimeoutMiddleware::default())
            .middleware(BodyLimitMiddleware::default());

        // MiddlewareStack doesn't expose count, but we can verify it's not empty
        assert!(!app.middlewares.is_empty());
    }

    #[test]
    fn test_rapina_introspection_enabled_in_debug() {
        let app = Rapina::new();
        // In debug builds, introspection should be enabled
        assert_eq!(app.introspection, cfg!(debug_assertions));
    }

    #[test]
    fn test_rapina_with_introspection_enabled() {
        let app = Rapina::new().with_introspection(true);
        assert!(app.introspection);
    }

    #[test]
    fn test_rapina_with_introspection_disabled() {
        let app = Rapina::new().with_introspection(false);
        assert!(!app.introspection);
    }

    #[test]
    fn test_rapina_with_health_check_enabled() {
        let app = Rapina::new().with_health_check(true);
        assert!(app.health_check);
    }

    #[test]
    fn test_rapina_with_health_check_disabled() {
        let app = Rapina::new().with_health_check(false);
        assert!(!app.health_check);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn test_rapina_with_metrics_enabled() {
        let app = Rapina::new().with_metrics(true);
        assert!(app.metrics);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn test_rapina_with_metrics_disabled() {
        let app = Rapina::new().with_metrics(false);
        assert!(!app.metrics);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn test_rapina_add_metric_stores_collector() {
        use prometheus::IntCounter;

        let counter = IntCounter::new("my_app_total", "My app counter").unwrap();
        let app = Rapina::new().enable_metrics().add_metric(counter);

        assert_eq!(app.custom_metrics.len(), 1);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn test_rapina_add_multiple_metrics() {
        use prometheus::{IntCounter, IntGauge};

        let c1 = IntCounter::new("metric_one", "first").unwrap();
        let c2 = IntCounter::new("metric_two", "second").unwrap();
        let g1 = IntGauge::new("metric_three", "third").unwrap();

        let app = Rapina::new()
            .enable_metrics()
            .add_metric(c1)
            .add_metric(c2)
            .add_metric(g1);

        assert_eq!(app.custom_metrics.len(), 3);
    }

    #[test]
    #[cfg(feature = "metrics")]
    fn test_rapina_custom_metrics_appear_in_output() {
        use crate::metrics::MetricsRegistry;
        use prometheus::IntCounter;

        let counter = IntCounter::new("orders_placed_total", "Orders placed").unwrap();
        counter.inc();
        counter.inc();

        let registry = MetricsRegistry::new_with_collectors(vec![Box::new(counter)]);
        let output = registry.encode();

        assert!(output.contains("orders_placed_total"));
        assert!(output.contains("Orders placed"));
    }

    #[test]
    #[should_panic(expected = "failed to register custom metric")]
    #[cfg(feature = "metrics")]
    fn test_add_metric_collision_with_builtin_panics() {
        use prometheus::IntCounter;

        // "http_requests_in_flight" is registered by Rapina internally;
        // a custom collector with the same name must panic during prepare().
        let collider =
            IntCounter::new("http_requests_in_flight", "collides with built-in").unwrap();

        Rapina::new()
            .enable_metrics()
            .add_metric(collider)
            .prepare();
    }

    #[test]
    #[should_panic(expected = "failed to register custom metric")]
    #[cfg(feature = "metrics")]
    fn test_add_metric_duplicate_custom_names_panics() {
        use prometheus::IntCounter;

        // Two user-supplied collectors sharing the same name must panic during prepare().
        let c1 = IntCounter::new("my_custom_events_total", "first").unwrap();
        let c2 = IntCounter::new("my_custom_events_total", "second").unwrap();

        Rapina::new()
            .enable_metrics()
            .add_metric(c1)
            .add_metric(c2)
            .prepare();
    }

    #[test]
    fn test_rapina_shutdown_timeout_default() {
        let app = Rapina::new();
        assert_eq!(app.shutdown_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_rapina_shutdown_timeout_custom() {
        let app = Rapina::new().shutdown_timeout(Duration::from_secs(10));
        assert_eq!(app.shutdown_timeout, Duration::from_secs(10));
    }

    #[test]
    fn test_rapina_on_shutdown_adds_hook() {
        let app = Rapina::new()
            .on_shutdown(|| async { println!("hook 1") })
            .on_shutdown(|| async { println!("hook 2") });
        assert_eq!(app.shutdown_hooks.len(), 2);
    }

    // ── jobs builder ─────────────────────────────────────────────────────────

    #[cfg(feature = "database")]
    mod jobs_tests {
        use std::time::Duration;

        use super::*;
        use crate::jobs::JobConfig;

        /// Worker is opt-in — must be `None` until `.jobs()` is called.
        #[test]
        fn jobs_config_none_by_default() {
            let app = Rapina::new();
            assert!(app.jobs_config.is_none());
        }

        /// `.jobs()` stores the config so the worker is spawned on `listen()`.
        #[test]
        fn jobs_sets_config() {
            let app = Rapina::new().jobs(JobConfig::default());
            assert!(app.jobs_config.is_some());
        }

        /// Calling `.jobs()` a second time replaces the previous config —
        /// last call wins, no silent merging.
        #[test]
        fn jobs_overrides_previous_config() {
            let app = Rapina::new()
                .jobs(JobConfig::default().batch_size(5))
                .jobs(JobConfig::default().batch_size(20));
            assert_eq!(app.jobs_config.unwrap().batch_size, 20);
        }

        /// All `JobConfig` fields survive the round-trip through the builder.
        #[test]
        fn jobs_config_fields_preserved() {
            let config = JobConfig::default()
                .poll_interval(Duration::from_secs(10))
                .batch_size(25)
                .queues(["emails", "heavy"])
                .job_timeout(Duration::from_secs(120));

            let app = Rapina::new().jobs(config);
            let stored = app.jobs_config.unwrap();

            assert_eq!(stored.poll_interval, Duration::from_secs(10));
            assert_eq!(stored.batch_size, 25);
            assert_eq!(stored.queues, vec!["emails", "heavy"]);
            assert_eq!(stored.job_timeout, Duration::from_secs(120));
        }

        /// `.jobs()` integrates cleanly with other builder methods —
        /// chaining order must not matter for unrelated fields.
        #[test]
        fn jobs_chains_with_other_builder_methods() {
            #[derive(Clone)]
            struct MyState;

            let app = Rapina::new()
                .state(MyState)
                .jobs(JobConfig::default())
                .shutdown_timeout(Duration::from_secs(10));

            assert!(app.jobs_config.is_some());
            assert!(app.state.get::<MyState>().is_some());
            assert_eq!(app.shutdown_timeout, Duration::from_secs(10));
        }
    }

    #[cfg(feature = "cron-scheduler")]
    #[test]
    fn test_rapina_cron_adds_cronjob() {
        let app = Rapina::new().cron("1/5 * * * * *", || async {
            println!("cronjob 1");
            Ok::<(), Error>(())
        });
        assert_eq!(app.cron_scheduler.unwrap().len(), 1);
    }

    #[cfg(feature = "cron-scheduler")]
    #[test]
    fn test_rapina_cron_adds_multiple_cronjobs() {
        let app = Rapina::new()
            .cron("1/5 * * * * *", || async {
                println!("cronjob 1");
                Ok::<(), Error>(())
            })
            .cron("1/5 * * * * *", || async {
                println!("cronjob 2");
                Ok::<(), Error>(())
            });
        assert_eq!(app.cron_scheduler.unwrap().len(), 2);
    }

    #[cfg(feature = "cron-scheduler")]
    #[test]
    #[should_panic]
    fn test_rapina_cron_broken_cronjob_panics() {
        let broken_schedule = "";
        Rapina::new().cron(broken_schedule, || async { Ok::<(), Error>(()) });
    }
}
