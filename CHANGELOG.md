# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **OpenTelemetry OTLP tracing exporter**: `with_telemetry(TelemetryConfig { endpoint, service_name, sample_rate })` exports traces over OTLP gRPC to a collector such as Jaeger or Datadog, behind the `otel` feature. Incoming W3C `traceparent` headers are honored so traces continue across service boundaries, request spans follow the HTTP semantic conventions and carry the response status, the OTel `trace_id`/`span_id` are recorded on the request span so logs correlate with the exported trace, and pending spans are flushed on graceful shutdown. Plaintext gRPC by default; enable `otel-tls` for `https://` collectors (#93).

### Changed
- `with_tracing` now installs the tracing subscriber when the server starts (in `listen`) rather than at call time, so it can be composed with the OTLP export layer onto a single global subscriber.

## [0.12.0] - 2026-05-15

### Added
- **Cursor-based pagination**: `CursorPaginate<V>` extractor and `CursorPaginated<T>` response for stable feeds and infinite scroll, alongside existing offset pagination (#540).
- **Streaming and SSE responses**: First-class streaming response types and Server-Sent Events support (#536).
- **`llms.txt` endpoint and CLI export**: `/llms.txt` route plus `rapina llms` command for AI-tool discovery (#528).
- **`Header<T>` typed extractor**: Strongly-typed header extraction with structured 400 errors (#542).
- **Custom Prometheus collectors**: `MetricsBuilder::add_metric()` for registering app-defined collectors (#538).
- **Opt-in auto-migrations**: `DatabaseConfig::auto_migrate(true)` and `DATABASE_AUTO_MIGRATE=true` apply pending migrations at startup (#547). Off by default; when off, `run_migrations` logs pending migration names and points to `rapina migrate up`.
- **Bundled docs and `AGENTS.md`**: `rapina new` drops `AGENTS.md`, `CLAUDE.md`, and `.rapina-docs/` into new projects so AI coding tools have framework-specific context. `rapina doctor --fix-agents` refreshes after upgrades (#535).

### Changed
- **Breaking:** `DatabaseConfig::new` and `from_env` default `auto_migrate` to `false`. Opt in with `.auto_migrate(true)` or `DATABASE_AUTO_MIGRATE=true` to apply migrations at startup.
- **Unified type system**: Consolidated type representation across codegen, schema, and OpenAPI layers (#519).
- **Cache invalidation: ancestor collection prefixes**: Mutations now invalidate every ancestor collection prefix, not just the exact route (#537).
- **Codegen: consolidated PK source of truth**: Single primary-key inference path across `import` and `add` (#546).
- **Skip rewrite of unchanged `generate_rapina_docs` output**: Avoids touch-time churn in source control (#564).
- **`tokio-tungstenite` 0.28 → 0.29, `hyper-tungstenite` 0.19 → 0.20** (#560).

### Fixed
- **`schema!` rejected unsigned integer types**: Now emits a clear compile-time error pointing at the offending column (#557).

## [0.11.0] - 2026-04-01

### Added
- **Tower compatibility layer**: `tower` feature flag with `TowerLayerMiddleware` (tower Layer → rapina Middleware adapter), `RapinaService` (rapina stack → tower Service adapter), and `.layer()` builder method.
- **NextService Clone support**: Tower layers requiring `Clone` on the inner service (e.g. tower-resilience, retry, circuit breaker) now work out of the box.

## [0.10.0] - 2026-03-16

### Added
- **Serde-based Path extraction**: `Path<T>` now uses a custom serde deserializer, supporting `Path<u64>`, `Path<(u64, String)>` tuples, and `Path<MyStruct>` structs from a single implementation
- **Database seeding**: `rapina seed load`, `rapina seed dump`, and `rapina seed generate` commands behind `seed-*` feature flags
- **Snapshot testing**: `response.assert_snapshot("name")` with automatic UUID/timestamp redaction, `--bless` mode for updating golden files
- **RFC 7807 Problem Details**: Standardized error responses with configurable `ErrorConfig`, per-request scoping via `task_local!`
- **Three-layer router**: Static route map for O(1) parameterless lookup, hot cache, and frozen radix trie
- **Router benchmarks**: Criterion benchmarks for router resolution performance
- **Configurable request logging**: Verbose mode with header/query/body-size logging, header redaction for sensitive values
- **`--force` flag for `import database`**: Re-import over existing generated files
- **Irregular plurals in codegen**: Handles words like `status`, `address`, `child` correctly in singularize/pluralize
- **UUID primary key support** in `schema!` macro
- **`put_named` and `delete_named`** convenience methods on Router
- **URL shortener example**: Full CRUD example with database, migrations, and tests

### Changed
- **`State<T>` wrapped in `Arc<T>`**: Removes the `Clone` bound on state types, `into_inner()` returns `Arc<T>`
- **Positional extractor convention**: Last handler argument uses `FromRequest` (consumes body), all others use `FromRequestParts` — replaces string-based classification
- **`PathParams` backed by `SmallVec`**: Stack-allocated for up to 4 parameters, zero heap allocation for typical routes
- **Compression gated behind feature flag**: `compression` feature (enabled by default)
- **Macro preserves `mut` on handler arguments**: Enables mutable extractors like `mut form: Multipart`

## [0.6.0] - 2026-02-22

### Added
- **Route Auto Discovery**: Routes are automatically registered via `inventory` — no more manual wiring in `main.rs`
- `toml` upgraded to 1.0 (TOML spec 1.1 support)

### Changed
- Updated `jsonwebtoken` to 10.3.0
- Updated `ctrlc` to 3.5.2
- GitHub Actions: auto-labeler for PRs, welcome message for first-time contributors
- Consolidated Discord links across documentation

## [0.2.0] - 2025-01-24

### Added
- **Authentication**: JWT authentication with "protected by default" approach
  - `#[public]` attribute for public routes
  - `CurrentUser` extractor for accessing authenticated user
  - `AuthConfig` for JWT configuration from environment
  - `TokenResponse` helper for login endpoints
- **Configuration**: Type-safe config with `#[derive(Config)]` macro
  - `#[env = "VAR_NAME"]` for environment variable binding
  - `#[default = "value"]` for default values
  - `load_dotenv()` helper for .env files
  - Fail-fast validation with clear error messages
- **Documentation**: Full docs site at userapina.com
  - Getting started guide
  - CLI reference
  - Philosophy section
- **CLI**: New commands
  - `rapina doctor` for health checks
  - `rapina routes` for route introspection

### Changed
- All routes now require authentication by default (use `#[public]` to opt-out)
- Improved error messages for missing configuration

## [0.1.0-alpha.3] - 2025-01-15

### Added
- OpenAPI 3.0 automatic generation
- CLI tools: `rapina openapi export`, `rapina openapi check`, `rapina openapi diff`
- Breaking change detection for API contracts
- Validation with `Validated<T>` extractor
- Observability with structured logging and tracing

## [0.1.0-alpha.2] - 2025-01-10

### Added
- Route introspection endpoint (`/__rapina/routes`)
- Test client for integration testing
- Middleware system (`Timeout`, `BodyLimit`, `TraceId`)

## [0.1.0-alpha.1] - 2025-01-05

### Added
- Initial release
- Basic router with path parameters
- Typed extractors (`Json`, `Path`, `Query`, `Form`, `Headers`, `State`)
- Standardized error handling with `trace_id`
- CLI (`rapina new`, `rapina dev`)

[Unreleased]: https://github.com/rapina-rs/rapina/compare/v0.12.0...HEAD
[0.12.0]: https://github.com/rapina-rs/rapina/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/rapina-rs/rapina/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/rapina-rs/rapina/compare/v0.9.0...v0.10.0
[0.6.0]: https://github.com/rapina-rs/rapina/compare/v0.5.0...v0.6.0
[0.2.0]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.3...v0.2.0
[0.1.0-alpha.3]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/rapina-rs/rapina/releases/tag/v0.1.0-alpha.1
