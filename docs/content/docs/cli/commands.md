+++
title = "Commands"
description = "Complete CLI command reference"
weight = 1
date = 2025-02-13
+++

## rapina new

Create a new Rapina project:

```bash
rapina new my-app
```

This creates:

- `Cargo.toml` with Rapina dependencies
- `src/main.rs` with a basic API
- `.gitignore`
- `README.md`
- `AGENTS.md` — Rapina-specific rules for AI agents, composed from feature-flagged fragments and stamped with a version + SHA256 hash
- `CLAUDE.md` — one-line pointer (`@AGENTS.md`) so Claude picks up the rules automatically
- `.cursor/rules` — Cursor rules
- `.rapina-docs/` — individual fragment files committed to the repo, version-matched to the installed CLI

`AGENTS.md` and `.rapina-docs/` contain the same Rapina-specific imperative rules (correct extractor ordering, `#[public]` requirement, validated bodies, typed errors, migration commands) so agents generate correct code out of the box without relying on stale training data.

### Options

| Flag                | Description                                                                       |
| ------------------- | --------------------------------------------------------------------------------- |
| `--template <T>`    | Starter template: `rest-api` (default), `crud`, `auth`                            |
| `--db <DB>`         | Database: `sqlite`, `postgres`, `mysql`. Required for `--template crud`           |
| `--no-ai`           | Skip all AI files (`AGENTS.md`, `CLAUDE.md`, `.cursor/rules`, `.rapina-docs/`)    |
| `--no-agents-md`    | Skip `AGENTS.md` and `CLAUDE.md` only                                             |
| `--no-bundled-docs` | Skip `.rapina-docs/` only                                                         |
| `--agents-md-only`  | Generate `AGENTS.md` and `CLAUDE.md` but skip `.rapina-docs/` and `.cursor/rules` |

Examples:

```bash
# REST API with SQLite
rapina new my-app --db sqlite

# CRUD template with PostgreSQL
rapina new my-app --template crud --db postgres

# No bundled docs (you maintain your own)
rapina new my-app --agents-md-only

# No AI files at all
rapina new my-app --no-ai
```

## rapina add resource

Scaffold a complete CRUD resource with handlers, DTOs, error type, entity definition, and a database migration:

```bash
rapina add resource user name:string email:string active:bool
```

This creates:

```
src/users/mod.rs           # Module declarations
src/users/handlers.rs      # list, get, create, update, delete handlers
src/users/dto.rs           # CreateUser, UpdateUser request types
src/users/error.rs         # UserError with IntoApiError + DocumentedError
src/entity.rs              # Appends a schema! {} block (or creates the file)
src/migrations/m{TS}_create_users.rs   # Pre-filled migration
src/migrations/mod.rs      # Updated with mod + migrations! macro entry
```

Fields use a `name:type` format. Supported types:

| Type            | Aliases       | Rust Type       | Column                       | Default |
| --------------- | ------------- | --------------- | ---------------------------- | ------- |
| `string`        |               | `String`        | VARCHAR                      | none    |
| `text`          |               | `String`        | TEXT                         | none    |
| `i32`           | `integer`     | `i32`           | INTEGER                      | none    |
| `i64`           | `bigint`      | `i64`           | BIGINT                       | none    |
| `f32`           | `float`       | `f32`           | FLOAT                        | none    |
| `f64`           | `double`      | `f64`           | DOUBLE                       | none    |
| `bool`          | `boolean`     | `bool`          | BOOLEAN                      | `false` |
| `uuid`          |               | `Uuid`          | UUID                         | none    |
| `datetime`      | `timestamptz` | `DateTime`      | TIMESTAMPTZ (timezone-aware) | none    |
| `naivedatetime` | `timestamp`   | `NaiveDateTime` | TIMESTAMP (without timezone) | none    |
| `date`          |               | `Date`          | DATE                         | none    |
| `decimal`       |               | `Decimal`       | DECIMAL                      | none    |
| `json`          |               | `Json`          | JSON                         | none    |

### Sensible defaults

`bool`/`boolean` columns always emit `DEFAULT FALSE` in the migration. This avoids requiring every insert to explicitly set the field when `false` is the natural starting state.

`created_at` and `updated_at` (`TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP`) are injected automatically into every generated migration and the entity's `schema!` block. These columns are required for SeaORM's `ActiveModelBehavior` and `before_save` hooks to work correctly.

To skip the timestamp columns (e.g., for a join table or audit log with custom timestamp logic):

```bash
rapina add resource user name:string email:string --no-timestamps
```

The generated handlers follow Rapina conventions and are ready to wire into your router. The command prints the exact code you need to add to `main.rs`:

```
  Next steps:

  1. Add the module declaration to src/main.rs:

     mod users;
     mod entity;
     mod migrations;

  2. Register the routes in your Router:

     use users::handlers::{list_users, get_user, create_user, update_user, delete_user};

     let router = Router::new()
         .get("/users", list_users)
         .get("/users/:id", get_user)
         .post("/users", create_user)
         .put("/users/:id", update_user)
         .delete("/users/:id", delete_user);

  3. Enable the database feature in Cargo.toml:

     rapina = { version = "...", features = ["postgres"] }
```

The resource name must be lowercase with underscores (e.g., `user`, `blog_post`). Pluralization is automatic. If the resource directory already exists, the command fails with a clear error instead of overwriting.

## rapina import database

Import schema from a live database, generating entities, migrations, handlers, DTOs, and error types for each table:

```bash
rapina import database --url postgres://user:pass@localhost/mydb
```

Options:

| Flag               | Description                                               | Default             |
| ------------------ | --------------------------------------------------------- | ------------------- |
| `--url <URL>`      | Database connection URL (or `DATABASE_URL` env)           | _required_          |
| `--tables <T1,T2>` | Only import specific tables (comma-separated)             | all tables          |
| `--schema <NAME>`  | Database schema name                                      | `public` (Postgres) |
| `--force`          | Overwrite existing files (re-import after schema changes) | false               |
| `--diff`           | Compare entities against the live database and report drift (writes nothing) | false               |

Supported databases: PostgreSQL (`postgres://`), MySQL (`mysql://`), SQLite (`sqlite://`). Each requires the corresponding feature:

```bash
cargo install rapina-cli --features import-postgres
cargo install rapina-cli --features import-mysql
cargo install rapina-cli --features import-sqlite
```

For each valid table, the command generates the same files as `rapina add resource`: a feature module (`src/<plural>/`), a `schema!` block in `src/entity.rs`, and a timestamped migration.

When both sides of a foreign key are being imported, the command generates relationship fields in the `schema!` block instead of plain integer columns:

- Non-nullable FK (`author_id NOT NULL → users.id`) → `author: User` (BelongsTo)
- Nullable FK (`author_id NULL → users.id`) → `author: Option<User>` (BelongsTo)
- Referenced table receives → `comments: Vec<Comment>` (HasMany)

Tables are skipped if they have no primary key, a composite primary key, or are internal migration tables (`seaql_migrations`, `sqlx_migrations`, `__diesel_schema_migrations`).

### Re-importing with `--force`

Without `--force`, the command errors if a feature module directory already exists. With `--force`:

- Existing `src/<plural>/` directories are removed and re-created
- Duplicate `schema!` blocks in `entity.rs` are replaced instead of appended
- A new migration file is always created (timestamps prevent collisions)

This is useful when the upstream database schema changes and you want to regenerate the Rapina code to match.

### Drift detection with `--diff`

`--diff` compares the `schema!` blocks in `src/entity.rs` against the live database and reports the differences instead of generating code. Use it to catch manual database changes that never made it back into the entities, or entities that no longer match the tables.

```bash
rapina import database --url postgres://user:pass@localhost/mydb --diff
```

It reports columns that exist in code but not in the database, columns in the database with no matching field, type and nullability mismatches, missing tables, and primary key differences. A database table with no entity is reported as a note, not drift, so tables you haven't modeled don't fail the check.

The exit codes suit CI: `0` when code and database agree, `2` on drift, `1` on an error such as a bad connection or unreadable `entity.rs`. `--tables` limits the comparison to specific tables. `--diff` writes nothing, so passing `--force` with it is an error.

Two cases are reported as notes rather than drift because they behave the same at runtime: a `String` field against a `text` column, and a `bool` field against a MySQL `tinyint` (MySQL has no native boolean). A field whose type has no `schema!` equivalent is skipped on both sides with a warning, so a hand-added enum column won't show as false drift.

## rapina dev

Start the development server with hot reload:

```bash
rapina dev
```

Options:

| Flag                | Description | Default   |
| ------------------- | ----------- | --------- |
| `-p, --port <PORT>` | Server port | 3000      |
| `--host <HOST>`     | Server host | 127.0.0.1 |

Example:

```bash
rapina dev -p 8080 --host 0.0.0.0
```

## rapina test

Run tests with pretty output:

```bash
rapina test
```

Options:

| Flag          | Description                                        |
| ------------- | -------------------------------------------------- |
| `--coverage`  | Generate coverage report (requires cargo-llvm-cov) |
| `-w, --watch` | Watch for changes and re-run tests                 |
| `--bless`     | Update snapshot files (golden-file testing)        |
| `[FILTER]`    | Filter tests by name                               |

Examples:

```bash
# Run all tests
rapina test

# Run tests matching a pattern
rapina test user

# Watch mode - re-run on file changes
rapina test -w

# Generate coverage report
rapina test --coverage

# Save or update response snapshots
rapina test --bless
```

Output:

```
  ✓ tests::it_works
  ✓ tests::user_creation
  ✗ tests::it_fails

──────────────────────────────────────────────────
FAIL 2 passed, 1 failed, 0 ignored
████████████████████████████░░░░░░░░░░░░
```

## rapina routes

List all registered routes from a running server:

```bash
rapina routes
```

Output:

```
  METHOD  PATH                  HANDLER
  ------  --------------------  ---------------
  GET     /                     hello
  GET     /health               health
  GET     /users/:id            get_user
  POST    /users                create_user

  4 route(s) registered
```

> **Note:** The server must be running for this command to work.

## rapina doctor

Run health checks on your project:

```bash
rapina doctor
```

Doctor runs two classes of checks:

**Local checks (no server required)**

`AGENTS.md` drift detection compares the on-disk block against the current bundled fragments. Three outcomes:

- `✓ AGENTS.md is up to date` — SHA256 of the block body matches what the current CLI would generate.
- `⚠ AGENTS.md is stale` — content is unchanged since it was last written (stored hash matches), but the current CLI would generate something different (version bumped, fragments changed). Fix with `--fix-agents`.
- `✗ AGENTS.md has been edited inside the markers` — someone edited content between the `BEGIN`/`END` markers. Rapina refuses to auto-fix; move your custom rules outside the markers first.

**API checks (requires a running server)**

- Response schemas defined for all routes
- Error documentation present
- OpenAPI metadata (descriptions)
- No duplicate handler paths
- `llms.txt` endpoint reachable (warns if disabled — enabled by default in debug builds, disabled in release builds; call `enable_llms_txt()` on your `App` to enable it in production)

Output:

```
  ✓ AGENTS.md is up to date
  → Running API health checks on http://127.0.0.1:3000...

  ✓ All routes have response schemas
  ✓ No duplicate handler paths
  ✓ llms.txt is enabled and reachable
  ⚠ Missing documentation: GET /users/:id
  ⚠ No documented errors: POST /users

  Summary: 3 passed, 2 warnings, 0 errors
```

### Options

| Flag           | Description                                                                        |
| -------------- | ---------------------------------------------------------------------------------- |
| `--fix-agents` | Refresh `AGENTS.md` from current bundled fragments                                 |
| `--force`      | With `--fix-agents`: overwrite even if the block has user edits inside the markers |

### Fixing a stale `AGENTS.md`

After a Rapina version bump or when fragments change:

```bash
rapina doctor --fix-agents
```

### Fixing a user-edited `AGENTS.md`

If you edited inside the markers and want to discard those edits:

```bash
rapina doctor --fix-agents --force
```

If you want to keep your custom content, move it outside the markers first:

```markdown
<!-- your custom rules here — above the managed block -->

<!-- BEGIN:rapina-agent-rules v0.11.0 sha256:... -->

...managed content, do not edit...

<!-- END:rapina-agent-rules -->

<!-- or below the managed block -->
```

Then run `rapina doctor --fix-agents`.

### Generating `AGENTS.md` for an existing project

Projects created before `0.11.0` have no `AGENTS.md`. Run once to generate:

```bash
rapina doctor --fix-agents
```

## rapina migrate new

Generate a new empty migration file:

```bash
rapina migrate new create_posts
```

This creates a timestamped migration file in `src/migrations/` and updates `mod.rs` with the module declaration and `migrations!` macro entry. The migration name must be lowercase with underscores.

> **Note:** `rapina add resource` already generates a pre-filled migration. Use `rapina migrate new` when you need a migration that isn't tied to a new resource (e.g., adding a column, creating an index).

## rapina jobs init

Set up the background jobs migration in your project:

```bash
rapina jobs init
```

This adds the framework's `create_rapina_jobs` migration to `src/migrations/mod.rs`. If the file doesn't exist, it creates one. If the migration is already configured, the command is a no-op.

The migration creates the `rapina_jobs` table used by the background jobs system. It uses a zero timestamp prefix so it always runs before your application migrations. See [Background Jobs](/docs/core-concepts/background-jobs/) for the full table schema and types.

> **Note:** The jobs migration requires PostgreSQL. It uses `gen_random_uuid()` and partial indexes, which are not available in MySQL or SQLite.

## rapina jobs list

Show job counts grouped by status:

```bash
rapina jobs list
```

Output:

```
  STATUS        COUNT
  ────────────  ─────
  pending       3
  running       1
  completed     42
  failed        2

  ✓ 48 total job(s)
```

Options:

| Flag       | Description                                         |
| ---------- | --------------------------------------------------- |
| `--failed` | Also list individual failed jobs with error details |

With `--failed`:

```bash
rapina jobs list --failed
```

This appends a table of failed jobs showing ID, queue, job type, attempt count (`attempts/max_retries`), and the last error message.

Requires the `jobs` feature:

```bash
cargo install rapina-cli --features jobs-postgres
```

## rapina llms export

Fetch the [`llms.txt`](/docs/core-concepts/llms-txt/) document from your running development server and write it to stdout or a file:

```bash
# Print to stdout
rapina llms export

# Write to file
rapina llms export -o llms.txt
```

Options:

| Flag                  | Description                               | Default   |
| --------------------- | ----------------------------------------- | --------- |
| `-o, --output <FILE>` | Output file                               | stdout    |
| `-p, --port <PORT>`   | Port to connect to (reads `$RAPINA_PORT`) | 3000      |
| `--host <HOST>`       | Host to connect to                        | 127.0.0.1 |

Requires a running server with llms.txt enabled. llms.txt is on by default in debug builds (`rapina dev`).

## rapina openapi export

Export the OpenAPI specification to a file:

```bash
rapina openapi export -o openapi.json
```

> When no output file is given, the spec is written to stdout.

Options:

| Flag                  | Description                                                               | Default   |
| --------------------- | ------------------------------------------------------------------------- | --------- |
| `-o, --output <FILE>` | Output file                                                               | stdout    |
| `-p, --port <PORT>`   | Port to connect to (reads `$RAPINA_PORT`, falling back to `$SERVER_PORT`) | 3000      |
| `--host <HOST>`       | Host to connect to                                                        | 127.0.0.1 |

## rapina openapi check

Verify that the committed spec matches the current code:

```bash
rapina openapi check
```

Useful in CI to ensure the spec is always up to date.

## rapina openapi diff

Detect breaking changes against another branch:

```bash
rapina openapi diff --base main
```

Output:

```
  Comparing OpenAPI spec with main branch...

  Breaking changes:
    - Removed endpoint: /health
    - Removed method: DELETE /users/{id}

  Non-breaking changes:
    - Added endpoint: /posts
    - Added field 'avatar' in GET /users/{id}

Error: Found 2 breaking change(s)
```

The command exits with code 1 if breaking changes are detected.
