# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Routine dependency-only updates are intentionally omitted unless they change user-facing behavior, APIs, generated output, security posture, or supported platforms.

## [Unreleased]

### Added
- **Standalone job workers**: `Worker` was `pub(crate)` and the only spawn site was inside `listen()`, so workers could not run as their own process — scaling them independently of the HTTP app meant booting a server, a port and a healthcheck for a process that only polls the database. `rapina::jobs::run_worker(db, config)` now runs them from any binary that links the crate defining the jobs, sharing nothing with the API but the database, and returns once every worker has drained so SIGINT/SIGTERM still finishes the batch in flight. Handlers come from `inventory` link-time registration, so the binary must compile them; `Db` resolves in handler arguments, `State<T>` does not (#794).
- **Per-queue background job configuration**: `poll_interval`, `batch_size`, and `job_timeout` were global, so a queue whose jobs are far heavier than the rest could not be paced differently — raising `job_timeout` for one slow handler lengthened the lease for every queue. `JobConfig::queue_configs()` now takes `QueueConfig` entries that override any of the three per queue, with unset fields inheriting the global value, and a queue named only there is subscribed automatically. Registering an override for a queue that `queues()` does not list is harmless and unused, so one config can be narrowed per process (#794).

### Changed
- **One job worker per subscribed queue**: a single worker used to poll every subscribed queue with one `queue IN (…)` statement. Because `LIMIT` and the lease interval are properties of a statement, per-queue `batch_size` and `job_timeout` are only expressible with one statement per queue, so `.jobs()` now spawns one worker per entry in `queues()`, each claiming with `queue = ?`. Workers share the application state and database pool, and the cost is one claim statement per queue per poll cycle instead of one for all of them (#794).
- **Job workers only reap their own queue**: the reaper had no queue predicate, so a worker subscribed to `default` also reclaimed stranded leases on queues it never polled. Both reaper statements are now scoped to the worker's queue. This is what makes running workers for different queues as separate processes safe, but it does remove the incidental cross-queue cleanup a single worker used to perform — a queue with no worker running will no longer have its expired leases reclaimed by someone else (#794).
- **`#[public]` must sit on a route handler**: `#[public]` on a function without `#[get]`, `#[post]`, `#[put]`, `#[patch]` or `#[delete]`, including `#[relay]` and `#[job]` handlers, used to do nothing and is now a compile error, and so is `#[public]` with arguments. `rapina::discovery::PublicMarker` is removed; a discovered route is public only when its own `RouteDescriptor::is_public` is set (#832).

### Fixed
- **Background jobs documentation**: Document supported database backends and schema mappings, clarify MySQL retry timestamp precision, and include the database driver in CLI installation instructions (#797).
- **Expired job leases are now reclaimed**: jobs left `running` by a crashed worker were stranded forever because every claim statement selected only `pending` rows. A reaper now runs before each claim: expired leases with retry budget left return to `pending`, counting the crashed run toward `attempts` exactly like a failure, and rows past `max_retries` are marked `failed`. Recovery latency is bounded by `job_timeout + poll_interval`, and the new `reap_indexes` migration indexes the scan. Retry, failure, and completion writes are fenced to `running` rows, so a worker whose lease was reclaimed mid-execution cannot overwrite the job's state afterwards. A job can still be executed twice when `job_timeout` is shorter than the batch's worst case (`batch_size` times the slowest handler), so size the timeout for the batch, not a single job (#793).
- **Panicking job handlers no longer kill the worker**: a panic used to unwind through the poll loop and stop all job processing. It is now caught, logged at `error` level, and routed through the same retry path (#793).

### Security
- **`#[public]` leaked to same-named handlers**: `#[public]` above a route macro registered the handler's bare function name, and discovery made every route with that name public. A protected handler sharing its name with a public one in another module skipped authentication. You were exposed if you put `#[public]` above a route macro on a function name used by another handler and never called `.openapi()`, which panics on duplicate handler names. Publicness now belongs to the route itself, and every discovered public route is logged at startup as `public route: METHOD /path` so you can audit it. `#[rapina::public]` below a route macro, which used to leave the route protected, now works too (#832).

## [0.13.1] - 2026-08-04

### Fixed
- **Column attributes on composite primary keys**: `#[column = "..."]` was ignored on any field named in `#[primary_key(...)]`, so the generated entity used the field name as the column name. `column_type` was dropped the same way, so a `Text` primary key landed as VARCHAR. If you had `#[column]` on a primary key field your tables were created with the field name, and you need a rename migration after upgrading (#750).

## [0.13.0] - 2026-06-25

### Added
- **Database import relationships**: `rapina import database` now turns single-column foreign keys into `BelongsTo` and `HasMany` relationship fields in generated `schema!` blocks when both sides of the relationship are imported (#548).
- **Handler descriptions in discovery output**: Route macros can now carry explicit descriptions, with rustdoc fallback, into `llms.txt` and OpenAPI output (#596).
- **OpenTelemetry OTLP tracing exporter**: `with_telemetry(TelemetryConfig { endpoint, service_name, sample_rate })` exports traces over OTLP gRPC to a collector such as Jaeger or Datadog, behind the `otel` feature. Incoming W3C `traceparent` headers are honored so traces continue across service boundaries, request spans follow the HTTP semantic conventions and carry the response status, the OTel `trace_id`/`span_id` are recorded on the request span so logs correlate with the exported trace, and pending spans are flushed on graceful shutdown. Plaintext gRPC by default; enable `otel-tls` for `https://` collectors (#607).
- **Redis TLS certificates**: `CacheConfig::redis_with_tls` and `RedisTlsConfig` support CA certificates plus client certificates and keys for `rediss://` Redis cache connections (#617).
- **`rapina doctor` `llms.txt` check**: `rapina doctor` now checks whether `/__rapina/llms.txt` is reachable and warns when it is disabled or unreachable (#618).
- **Multi-database background jobs**: Background job persistence now supports MySQL and SQLite alongside PostgreSQL, with backend-specific SQL (insert, claim, retry, success/fail) dispatched by `DbBackend` and UUIDs parsed from text on SQLite (#631).
- **`rapina import database --diff`**: Compares the `schema!` blocks in `src/entity.rs` against the live database and reports drift, columns on only one side, type and nullability mismatches, missing tables, and primary-key differences. Exits 0 when in sync, 2 on drift, and 1 on error for CI use; `--tables` scopes the comparison, round-trip-safe differences (`String` vs `text`, `bool` vs MySQL `tinyint`) are reported as notes, and combining it with `--force` is rejected since `--diff` writes nothing (#632).

### Changed
- **Native CLI HTTP client**: `rapina doctor`, `rapina routes`, `rapina openapi`, and `rapina llms` use an internal `ureq` client instead of shelling out to `curl` (#586).
- **Generated agent rules**: `AGENTS.md` and bundled agent docs generated by the CLI now mention the `llms.txt` endpoint (#580).
- **Metrics route labels**: Prometheus metrics now label requests by the matched route pattern, preserving route parameter names and using `<unmatched>` for 404s, instead of normalizing only numeric path segments (#634).
- **Redis cache construction**: `RedisCache::connect` now accepts optional TLS configuration; pass `None` for existing plaintext connections (#617).
- **Tracing subscriber startup**: `with_tracing` now installs the tracing subscriber when the server starts (in `listen`) rather than at call time, so it can be composed with the OTLP export layer onto a single global subscriber (#607).

### Fixed
- **Workspace-inherited Rapina features**: `AGENTS.md` drift detection and `rapina doctor --fix-agents` now resolve Rapina features inherited from `[workspace.dependencies]`, so workspace projects get the correct database, websocket, and jobs fragments (#561).
- **Validation error serialization**: `Validated<Json<T>>` and `Validated<Form<T>>` now log serialization failures instead of silently dropping validation details (#583).
- **Duplicate OpenAPI operation IDs**: OpenAPI spec generation now fails at build time when two public routes produce the same `operationId`, avoiding invalid specs that fail later (#593).
- **JWT audience validation**: JWT validation now fails fast when `validate_aud = true` is configured without an audience, and readiness checks report the misconfiguration before traffic is served (#573).

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

[Unreleased]: https://github.com/rapina-rs/rapina/compare/v0.13.1...HEAD
[0.13.1]: https://github.com/rapina-rs/rapina/compare/v0.13.0...v0.13.1
[0.13.0]: https://github.com/rapina-rs/rapina/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/rapina-rs/rapina/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/rapina-rs/rapina/compare/v0.10.0...v0.11.0
[0.10.0]: https://github.com/rapina-rs/rapina/compare/v0.9.0...v0.10.0
[0.6.0]: https://github.com/rapina-rs/rapina/compare/v0.5.0...v0.6.0
[0.2.0]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.3...v0.2.0
[0.1.0-alpha.3]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.2...v0.1.0-alpha.3
[0.1.0-alpha.2]: https://github.com/rapina-rs/rapina/compare/v0.1.0-alpha.1...v0.1.0-alpha.2
[0.1.0-alpha.1]: https://github.com/rapina-rs/rapina/releases/tag/v0.1.0-alpha.1
