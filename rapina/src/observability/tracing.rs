use tracing::Level;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::{SubscriberInitExt, TryInitError};
use tracing_subscriber::{EnvFilter, Layer, Registry, fmt};

/// Configuration for the tracing/logging system.
///
/// Use the builder pattern to configure logging output format and level.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// // JSON logging for production
/// Rapina::new()
///     .with_tracing(TracingConfig::new().json())
///     .router(router)
///     .listen("127.0.0.1:3000")
///     .await
/// ```
#[derive(Debug, Clone)]
pub struct TracingConfig {
    /// Output logs as JSON.
    pub json: bool,
    /// The minimum log level.
    pub level: Level,
    /// Include the target (module path) in logs.
    pub with_target: bool,
    /// Include the source file in logs.
    pub with_file: bool,
    /// Include line numbers in logs.
    pub with_line_number: bool,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            json: false,
            level: Level::INFO,
            with_target: true,
            with_file: false,
            with_line_number: false,
        }
    }
}

impl TracingConfig {
    /// Creates a new tracing configuration with default values.
    pub fn new() -> Self {
        Self::default()
    }

    /// Enables JSON output format.
    pub fn json(mut self) -> Self {
        self.json = true;
        self
    }

    /// Sets the minimum log level.
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Configures whether to include the target in logs.
    pub fn with_target(mut self, enabled: bool) -> Self {
        self.with_target = enabled;
        self
    }

    /// Configures whether to include file names in logs.
    pub fn with_file(mut self, enabled: bool) -> Self {
        self.with_file = enabled;
        self
    }

    /// Configures whether to include line numbers in logs.
    pub fn with_line_number(mut self, enabled: bool) -> Self {
        self.with_line_number = enabled;
        self
    }

    /// Initializes the tracing subscriber with this configuration.
    pub fn init(self) {
        let _ = init_subscriber(Some(self), None);
    }
}

/// Builds the stdout formatting layer for the given configuration.
fn fmt_layer(config: &TracingConfig) -> Box<dyn Layer<Registry> + Send + Sync> {
    let layer = fmt::layer()
        .with_target(config.with_target)
        .with_file(config.with_file)
        .with_line_number(config.with_line_number);

    if config.json {
        layer.json().boxed()
    } else {
        layer.boxed()
    }
}

/// Installs the global tracing subscriber.
///
/// Composes the stdout formatting layer with an optional extra layer (the OTLP
/// export layer when telemetry is configured) onto a single `Registry`, since a
/// global subscriber can only be set once. Does nothing when neither a tracing
/// config nor an extra layer is given. Uses `try_init` so a second call (for
/// example a direct `TracingConfig::init`) degrades to a no-op instead of
/// panicking.
pub(crate) fn init_subscriber(
    tracing: Option<TracingConfig>,
    extra_layer: Option<Box<dyn Layer<Registry> + Send + Sync>>,
) -> Result<(), TryInitError> {
    if tracing.is_none() && extra_layer.is_none() {
        return Ok(());
    }

    let config = tracing.unwrap_or_default();
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(config.level.to_string()));

    let mut layers: Vec<Box<dyn Layer<Registry> + Send + Sync>> = vec![fmt_layer(&config)];
    if let Some(layer) = extra_layer {
        layers.push(layer);
    }

    Registry::default()
        .with(layers.with_filter(filter))
        .try_init()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracing_config_default() {
        let config = TracingConfig::default();
        assert!(!config.json);
        assert_eq!(config.level, Level::INFO);
        assert!(config.with_target);
        assert!(!config.with_file);
        assert!(!config.with_line_number);
    }

    #[test]
    fn test_tracing_config_new() {
        let config = TracingConfig::new();
        assert!(!config.json);
    }

    #[test]
    fn test_tracing_config_json() {
        let config = TracingConfig::new().json();
        assert!(config.json);
    }

    #[test]
    fn test_tracing_config_level() {
        let config = TracingConfig::new().level(Level::DEBUG);
        assert_eq!(config.level, Level::DEBUG);
    }

    #[test]
    fn test_tracing_config_builder_chain() {
        let config = TracingConfig::new()
            .json()
            .level(Level::TRACE)
            .with_target(false)
            .with_file(true)
            .with_line_number(true);

        assert!(config.json);
        assert_eq!(config.level, Level::TRACE);
        assert!(!config.with_target);
        assert!(config.with_file);
        assert!(config.with_line_number);
    }
}
