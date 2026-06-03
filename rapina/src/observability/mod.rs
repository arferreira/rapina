//! Observability utilities for Rapina applications.
//!
//! This module provides tools for logging, tracing, and monitoring.

#[cfg(feature = "otel")]
mod telemetry;
mod tracing;

#[cfg(feature = "otel")]
pub use self::telemetry::TelemetryConfig;
#[cfg(feature = "otel")]
pub(crate) use self::telemetry::build_otel_pipeline;
pub use self::tracing::TracingConfig;
pub(crate) use self::tracing::init_subscriber;
