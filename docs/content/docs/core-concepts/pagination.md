+++
title = "Pagination"
description = "Built-in pagination for list endpoints"
weight = 6
date = 2025-02-27
+++

List endpoints come in two shapes: page-numbered (`?page=2&per_page=20`) and cursor-based (`?after=<token>&limit=20`). Rapina ships both as first-class primitives. Use the offset-style `Paginate` / `Paginated<T>` for random page access, and `CursorPaginate` / `CursorPaginated<T>` for stable feeds and infinite scroll.

## Quick Start

```rust
use rapina::prelude::*;
use rapina::database::Db;

#[get("/users")]
async fn list_users(db: Db, page: Paginate) -> Result<Paginated<user::Model>> {
    page.exec(User::find(), db.conn()).await
}
```

That's it. The extractor reads `?page=1&per_page=20` from the query string, `exec` runs fetch and count concurrently, and `Paginated<T>` serializes the response with metadata.

## The Paginate Extractor

`Paginate` implements `FromRequestParts` and parses two optional query parameters:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `page` | 1 | Page number (1-indexed) |
| `per_page` | 20 | Items per page |

Returns **422 Validation Error** when:
- `page` < 1
- `per_page` < 1
- `per_page` exceeds the configured maximum (default: 100)

```rust
#[get("/posts")]
async fn list_posts(db: Db, page: Paginate) -> Result<Paginated<post::Model>> {
    let select = Post::find()
        .filter(post::Column::Published.eq(true))
        .order_by_desc(post::Column::CreatedAt);

    page.exec(select, db.conn()).await
}
```

You can apply any SeaORM filters, ordering, or joins before passing the `Select` to `exec`.

## Response Shape

`Paginated<T>` implements `IntoResponse` directly, so you don't need to wrap it in `Json<>`. The response body looks like:

```json
{
  "data": [{ "id": 1, "name": "Alice" }, { "id": 2, "name": "Bob" }],
  "page": 1,
  "per_page": 20,
  "total": 42,
  "total_pages": 3,
  "has_prev": false,
  "has_next": true
}
```

`Paginated<T>` also derives `JsonSchema`, so it shows up correctly in OpenAPI output.

## Configuration

By default, `Paginate` uses `per_page=20` with a maximum of `100`. Override these by registering a `PaginationConfig` in your app state:

```rust
use rapina::prelude::*;

Rapina::new()
    .state(PaginationConfig {
        default_per_page: 25,
        max_per_page: 50,
    })
    // ...
```

If no config is registered, the hardcoded defaults apply. No setup required for the common case.

## Examples

### Basic list endpoint

The simplest case — paginate an entire table:

```rust
use rapina::prelude::*;
use rapina::database::Db;
use entity::user::{self, Entity as User};

#[get("/users")]
async fn list_users(db: Db, page: Paginate) -> Result<Paginated<user::Model>> {
    page.exec(User::find(), db.conn()).await
}
```

```
GET /users              → page 1, 20 items
GET /users?page=3       → page 3, 20 items
GET /users?per_page=50  → page 1, 50 items
```

### Filtering and ordering

Build your query however you want, then hand it to `exec`:

```rust
use rapina::sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

#[get("/posts")]
async fn list_posts(db: Db, page: Paginate) -> Result<Paginated<post::Model>> {
    let select = Post::find()
        .filter(post::Column::Published.eq(true))
        .order_by_desc(post::Column::CreatedAt);

    page.exec(select, db.conn()).await
}
```

### Combining with other extractors

`Paginate` is a `FromRequestParts` extractor, so it composes with everything else:

```rust
#[derive(Deserialize)]
struct UserFilter {
    role: Option<String>,
    active: Option<bool>,
}

#[get("/users")]
async fn list_users(
    db: Db,
    page: Paginate,
    query: Query<UserFilter>,
) -> Result<Paginated<user::Model>> {
    let mut select = User::find();

    if let Some(role) = &query.role {
        select = select.filter(user::Column::Role.eq(role.clone()));
    }
    if let Some(active) = query.active {
        select = select.filter(user::Column::Active.eq(active));
    }

    page.exec(select, db.conn()).await
}
```

```
GET /users?role=admin&page=2&per_page=10
```

### Mapping to a response DTO

Use `.map()` to transform models into response types. Pagination metadata carries over automatically:

```rust
#[derive(Serialize, JsonSchema)]
struct UserResponse {
    id: i32,
    name: String,
    email: String,
}

impl From<user::Model> for UserResponse {
    fn from(m: user::Model) -> Self {
        Self { id: m.id, name: m.name, email: m.email }
    }
}

#[get("/users")]
async fn list_users(db: Db, page: Paginate) -> Result<Paginated<UserResponse>> {
    Ok(page.exec(User::find(), db.conn()).await?.map(UserResponse::from))
}
```

Works with closures too:

```rust
#[get("/users")]
async fn list_users(db: Db, page: Paginate) -> Result<Paginated<String>> {
    Ok(page.exec(User::find(), db.conn()).await?.map(|u| u.name))
}
```

### Scoped to a parent resource

Pagination works the same with relationship queries:

```rust
#[get("/users/:id/posts")]
async fn list_user_posts(
    id: Path<i32>,
    db: Db,
    page: Paginate,
) -> Result<Paginated<post::Model>> {
    let select = Post::find()
        .filter(post::Column::AuthorId.eq(*id))
        .order_by_desc(post::Column::CreatedAt);

    page.exec(select, db.conn()).await
}
```

### Custom per_page limits

For endpoints with heavier payloads, register a global config:

```rust
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let db_config = DatabaseConfig::from_env()?;

    Rapina::new()
        .with_database(db_config).await?
        .state(PaginationConfig {
            default_per_page: 25,
            max_per_page: 50,
        })
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
```

Any request with `?per_page=51` now returns a 422.

## Performance

`exec` runs the data fetch and count queries **concurrently** using `tokio::join!`, not sequentially. Two queries hit the database in parallel, so latency is the cost of whichever query is slower, not both combined.

The `Select<E>` is cloned before splitting into fetch and count paginators. SeaORM query builders are cheap to clone (they're just AST nodes, not connections).

## Cursor Pagination

For high-traffic feeds, infinite-scroll lists, and any endpoint where rows can shift while a user is paging through them, cursor pagination is the better fit. Rapina ships `CursorPaginate<V>` and `CursorPaginated<T>` alongside the offset variant.

### When to choose which

| | Offset pagination | Cursor pagination |
|---|---|---|
| URL shape | `?page=2&per_page=20` | `?after=<token>&limit=20` |
| Total count | Yes (`total`, `total_pages`) | No (intentional) |
| Stable across writes | No, rows can shift between pages | Yes |
| Random page access | Yes, jump to page N | No, walk forward or backward |
| Cost on large tables | Grows with offset | Constant per page |

Reach for offset when the dataset is small, mostly static, and users want to land directly on page 47. Reach for cursor when the data is a feed, a timeline, an audit log, or anything where consistency between fetches matters more than knowing the total.

### Quick start

Declare the entity's default cursor column once with the `CursorKey` trait, then handlers stay terse:

```rust
use rapina::prelude::*;
use rapina::database::Db;
use rapina::pagination::{CursorKey, CursorPaginate, CursorPaginated};
use entity::user::{self, Entity as User};

impl CursorKey for user::Model {
    type Column = user::Column;
    type Value = i32;
    const COLUMN: user::Column = user::Column::Id;
    fn cursor_value(&self) -> i32 { self.id }
}

#[derive(Serialize, JsonSchema)]
struct UserResponse {
    id: i32,
    name: String,
}

impl From<user::Model> for UserResponse {
    fn from(m: user::Model) -> Self {
        Self { id: m.id, name: m.name }
    }
}

#[get("/users")]
async fn list_users(
    db: Db,
    cursor: CursorPaginate<i32>,
) -> Result<CursorPaginated<UserResponse>> {
    Ok(cursor
        .exec(User::find(), db.conn())
        .await?
        .map(UserResponse::from))
}
```

### Query parameters

| Parameter | Default | Description |
|-----------|---------|-------------|
| `after` | none | Opaque token; return rows after this position |
| `before` | none | Opaque token; return rows before this position |
| `limit` | 20 | Items per page (capped by `PaginationConfig::max_per_page`) |

Returns **422 Validation Error** when:
- `after` and `before` are both supplied
- `limit` is `0` or above the cap
- a token fails to base64-decode or fails to deserialize as `V`

### Response shape

```json
{
  "data": [{ "id": 11, "name": "Kim" }, { "id": 12, "name": "Lee" }],
  "next_cursor": "MTI",
  "prev_cursor": null
}
```

Tokens are URL-safe unpadded base64. `next_cursor` is `null` on the last page; `prev_cursor` is `null` on the very first page (when neither `?after=` nor `?before=` was supplied). Clients should round-trip the strings verbatim.

### `CursorKey` and one-off cursors

`exec` reads the default cursor column from the entity's `CursorKey` impl. For a one-off cursor on a non-default column (for example `email` in a search-by-email endpoint), reach for `exec_by` and pass the column and accessor at the call site:

```rust
#[get("/users/by-email")]
async fn list_users_by_email(
    db: Db,
    cursor: CursorPaginate<String>,
) -> Result<CursorPaginated<user::Model>> {
    cursor
        .exec_by(User::find(), user::Column::Email, db.conn(), |u| u.email.clone())
        .await
}
```

The closure must return the value of `column` for the given row, otherwise pagination will not point where the client expects. SeaORM's `Model` has no generic "get value of column" accessor, so the column and the accessor must be supplied together.

### Composite cursors

Currently single-column only. Composite cursors (e.g. `(created_at, id)` for stable ordering across rows with the same timestamp) are a non-breaking addition planned for a later release.

### Configuration

`CursorPaginate` shares `PaginationConfig` with offset pagination: `default_per_page` becomes the default `limit`, `max_per_page` enforces the cap.
