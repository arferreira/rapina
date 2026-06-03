//! # Rapina
//!
//! A fast, type-safe web framework for Rust inspired by FastAPI.
//!
//! Rapina focuses on **productivity**, **type safety**, and **clear conventions**,
//! making it easy to build production-ready APIs.
//!
//! ## Features
//!
//! - **Type-safe extractors** - Parse request data with compile-time guarantees
//! - **Declarative routing** - Use proc macros like `#[get]`, `#[post]` for clean route definitions
//! - **Middleware system** - Composable middleware with async support
//! - **Structured errors** - Standardized error responses with `trace_id` for debugging
//! - **Validation** - Built-in request validation using the `validator` crate
//! - **Observability** - Integrated tracing for structured logging
//!
//! ## Quick Start
//!
//! ```rust,ignore
//! use rapina::prelude::*;
//!
//! #[get("/")]
//! async fn hello() -> &'static str {
//!     "Hello, Rapina!"
//! }
//!
//! #[get("/users/:id")]
//! async fn get_user(id: Path<u64>) -> Result<Json<serde_json::Value>> {
//!     let id = id.into_inner();
//!     Ok(Json(serde_json::json!({ "id": id })))
//! }
//!
//! #[tokio::main]
//! async fn main() -> std::io::Result<()> {
//!     let router = Router::new()
//!         .get("/", hello)
//!         .get("/users/:id", get_user);
//!
//!     Rapina::new()
//!         .router(router)
//!         .listen("127.0.0.1:3000")
//!         .await
//! }
//! ```
//!
//! ## Extractors
//!
//! Rapina provides several extractors for parsing request data:
//!
//! - [`Json`](extract::Json) - Parse JSON request bodies
//! - [`Path`](extract::Path) - Extract path parameters
//! - [`Query`](extract::Query) - Parse query string parameters
//! - [`Form`](extract::Form) - Parse URL-encoded form data
//! - [`Headers`](extract::Headers) - Access request headers
//! - [`Cookie`](extract::Cookie) - Extract and deserialize cookies
//! - [`State`](extract::State) - Access application state
//! - [`Context`](extract::Context) - Access request context with trace_id
//! - [`Validated`](extract::Validated) - Validate extracted data
//! - [`Multipart`](extract::Multipart) - Parse multipart form data (e.g. file uploads)
//! - [`JsonWebToken`](jwt::JsonWebToken) - Parse and validate Json Web Tokens with configurable JWKS

//!
//! ## Middleware
//!
//! Built-in middleware for common use cases:
//!
//! - [`TimeoutMiddleware`](middleware::TimeoutMiddleware) - Request timeout handling
//! - [`BodyLimitMiddleware`](middleware::BodyLimitMiddleware) - Limit request body size
//! - [`TraceIdMiddleware`](middleware::TraceIdMiddleware) - Add trace IDs to requests
//! - [`RequestLogMiddleware`](middleware::RequestLogMiddleware) - Structured request logging
//! - [`RateLimitMiddleware`](middleware::RateLimitMiddleware) - Token bucket rate limiting (requires `rate-limit` feature)
//!
//! ## Introspection
//!
//! Access route metadata for documentation and tooling:
//!
//! - [`RouteInfo`](introspection::RouteInfo) - Metadata about registered routes
//!
//! ## Testing
//!
//! Integration testing utilities:
//!
//! - [`TestClient`](testing::TestClient) - Test client for integration testing

pub mod app;
pub mod auth;
pub mod cache;
#[cfg(feature = "cache-redis")]
pub mod cache_redis;
pub mod config;
pub mod context;
#[cfg(feature = "cron-scheduler")]
pub(crate) mod cron_scheduler;
#[cfg(feature = "database")]
pub mod database;
pub(crate) mod date_cache;
pub mod discovery;
pub mod error;
pub mod extract;
pub mod handler;
pub mod health;
pub mod introspection;
#[cfg(feature = "database")]
pub mod jobs;
#[cfg(feature = "jwks")]
pub mod jwt;
#[cfg(feature = "metrics")]
pub mod metrics;
pub mod middleware;
#[cfg(feature = "database")]
pub mod migration;
pub mod observability;
pub mod openapi;
#[cfg(feature = "database")]
pub mod pagination;
#[cfg(feature = "websocket")]
pub mod relay;
pub mod response;
pub mod router;
pub mod server;
pub mod state;
pub mod test;
pub mod testing;
#[cfg(feature = "websocket")]
pub mod websocket;

/// Convenient re-exports for common Rapina types.
///
/// This module re-exports the most commonly used types so you can
/// import everything you need with a single `use` statement:
///
/// ```
/// use rapina::prelude::*;
/// ```
pub mod prelude {
    pub use crate::app::Rapina;
    pub use crate::auth::{AuthConfig, CurrentUser, TokenResponse};
    pub use crate::cache::{CacheConfig, CacheMiddleware};
    pub use crate::config::{
        ConfigError, get_env, get_env_or, get_env_parsed, get_env_parsed_or, load_dotenv,
    };
    pub use crate::context::RequestContext;
    pub use crate::error::{DocumentedError, Error, ErrorVariant, IntoApiError, Result};
    pub use crate::extract::{
        Context, Cookie, Form, Header, Headers, Json, Path, Query, State, Validated,
    };
    #[cfg(feature = "multipart")]
    pub use crate::extract::{Field, Multipart};
    pub use crate::introspection::RouteInfo;
    #[cfg(feature = "database")]
    pub use crate::jobs::{JobDescriptor, JobId, JobRequest, JobResult, JobRow, JobStatus, Jobs};
    #[cfg(feature = "jwks")]
    pub use crate::jwt::{JsonWebToken, JwksClient};
    #[cfg(feature = "rate-limit")]
    pub use crate::middleware::{KeyExtractor, RateLimitConfig};
    pub use crate::middleware::{Middleware, Next, RequestLogConfig};
    #[cfg(feature = "tower")]
    pub use crate::middleware::{RapinaService, TowerLayerMiddleware};
    #[cfg(feature = "otel")]
    pub use crate::observability::TelemetryConfig;
    pub use crate::observability::TracingConfig;
    #[cfg(feature = "database")]
    pub use crate::pagination::{
        CursorKey, CursorPaginate, CursorPaginated, Paginate, Paginated, PaginationConfig,
    };
    #[cfg(feature = "websocket")]
    pub use crate::relay::{Relay, RelayConfig, RelayEvent};
    pub use crate::response::{IntoResponse, StaticStr};
    pub use crate::router::Router;

    pub use http::{Method, StatusCode};
    pub use schemars::JsonSchema;
    pub use serde::{Deserialize, Serialize};
    pub use tracing;
    pub use validator::Validate;

    pub use rapina_macros::{Config, delete, get, job, patch, post, public, put, relay, schema};
}

// Re-export proc macros at crate root so they work as rapina::schema!, rapina::get!, etc.
pub use rapina_macros::{Config, delete, get, job, patch, post, public, put, relay, schema};

// Re-export dependencies so users don't need to add them to their Cargo.toml
pub use http;
pub use hyper;
#[cfg(feature = "metrics")]
pub use prometheus;
pub use rust_decimal;
pub use schemars;
pub use serde_json;
pub use uuid;
pub use validator;

#[doc(hidden)]
pub use inventory;
#[doc(hidden)]
pub use openapi::openapi_schema_for;

#[cfg(feature = "websocket")]
pub use futures_util;
#[cfg(feature = "websocket")]
pub use tokio_tungstenite;

// Re-export sea-orm when database feature is enabled
#[cfg(feature = "database")]
pub use async_trait;
#[cfg(feature = "database")]
pub use sea_orm;
#[cfg(feature = "database")]
pub use sea_orm_migration;
