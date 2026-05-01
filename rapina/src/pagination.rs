//! Built-in pagination for database-backed list endpoints.
//!
//! Two flavors are provided, both feature-gated behind `database`:
//!
//! - [`Paginate`] / [`Paginated`] for offset/limit (`?page=&per_page=`),
//!   carrying total counts and page numbers.
//! - [`CursorPaginate`] / [`CursorPaginated`] for cursor pagination
//!   (`?after=&before=&limit=`), with opaque tokens and no count query.
//!
//! Both share [`PaginationConfig`] for `default_per_page` and `max_per_page`.
//!
//! # Offset
//!
//! ```rust,ignore
//! use rapina::prelude::*;
//! use rapina::database::Db;
//! use entity::user::{self, Entity as User};
//!
//! #[get("/users")]
//! async fn list_users(db: Db, page: Paginate) -> Result<Paginated<user::Model>> {
//!     page.exec(User::find(), db.conn()).await
//! }
//! ```
//!
//! # Cursor
//!
//! Implement [`CursorKey`] once per entity to declare the default cursor
//! column. Handlers then need only the query and the connection.
//!
//! ```rust,ignore
//! use rapina::prelude::*;
//! use rapina::database::Db;
//! use rapina::pagination::CursorKey;
//! use entity::user::{self, Entity as User};
//!
//! impl CursorKey for user::Model {
//!     type Column = user::Column;
//!     type Value = i32;
//!     const COLUMN: user::Column = user::Column::Id;
//!     fn cursor_value(&self) -> i32 { self.id }
//! }
//!
//! #[get("/users")]
//! async fn list_users(db: Db, cursor: CursorPaginate<i32>) -> Result<CursorPaginated<user::Model>> {
//!     cursor.exec(User::find(), db.conn()).await
//! }
//! ```
//!
//! For a one-off cursor on a non-default column, use
//! [`CursorPaginate::exec_by`].
//!
//! # Configuration
//!
//! Register [`PaginationConfig`] via `.state()` to override defaults:
//!
//! ```rust,ignore
//! Rapina::new()
//!     .state(PaginationConfig {
//!         default_per_page: 25,
//!         max_per_page: 50,
//!     })
//!     // ...
//! ```

use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as BASE64;
use schemars::JsonSchema;
use sea_orm::sea_query::IntoValueTuple;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, Select};
use serde::{Deserialize, Serialize};

use crate::database::DbError;
use crate::error::Error;
use crate::extract::{FromRequestParts, PathParams};
use crate::response::{BoxBody, IntoResponse};
use crate::state::AppState;

const DEFAULT_PER_PAGE: u64 = 20;
const DEFAULT_MAX_PER_PAGE: u64 = 100;

/// Global pagination defaults. Register via `.state(PaginationConfig { .. })`.
///
/// When not registered, hardcoded defaults apply (per_page=20, max=100).
#[derive(Debug, Clone)]
pub struct PaginationConfig {
    /// Default items per page when `per_page` is omitted (default: 20).
    pub default_per_page: u64,
    /// Maximum allowed `per_page` value (default: 100).
    pub max_per_page: u64,
}

impl Default for PaginationConfig {
    fn default() -> Self {
        Self {
            default_per_page: DEFAULT_PER_PAGE,
            max_per_page: DEFAULT_MAX_PER_PAGE,
        }
    }
}

/// Raw query params for deserialization. Both fields optional so missing
/// params fall back to defaults rather than returning a parse error.
#[derive(Deserialize)]
struct PaginateQuery {
    page: Option<u64>,
    per_page: Option<u64>,
}

/// Pagination extractor. Reads `?page=&per_page=` from the query string.
///
/// Returns 422 when values are invalid (page < 1, per_page < 1,
/// per_page > max). Respects [`PaginationConfig`] from app state if present,
/// otherwise uses hardcoded defaults.
#[derive(Debug, Clone, Copy)]
pub struct Paginate {
    pub page: u64,
    pub per_page: u64,
}

impl FromRequestParts for Paginate {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let query_str = parts.uri.query().unwrap_or("");
        let raw: PaginateQuery = serde_urlencoded::from_str(query_str)
            .map_err(|e| Error::validation(format!("invalid pagination params: {}", e)))?;

        let config = state.get::<PaginationConfig>();
        let default_per_page = config.map_or(DEFAULT_PER_PAGE, |c| c.default_per_page);
        let max_per_page = config.map_or(DEFAULT_MAX_PER_PAGE, |c| c.max_per_page);

        let page = raw.page.unwrap_or(1);
        let per_page = raw.per_page.unwrap_or(default_per_page);

        if page < 1 {
            return Err(Error::validation("page must be >= 1"));
        }
        if per_page < 1 {
            return Err(Error::validation("per_page must be >= 1"));
        }
        if per_page > max_per_page {
            return Err(Error::validation(format!(
                "per_page must be <= {}",
                max_per_page
            )));
        }

        Ok(Paginate { page, per_page })
    }
}

impl Paginate {
    /// Runs a paginated query: fetches the requested page and counts total
    /// items concurrently via `tokio::join!`.
    pub async fn exec<E>(
        &self,
        select: Select<E>,
        conn: &sea_orm::DatabaseConnection,
    ) -> Result<Paginated<E::Model>, Error>
    where
        E: EntityTrait,
        E::Model: Serialize + Send + Sync,
    {
        let paginator = select.clone().paginate(conn, self.per_page);
        let count_paginator = select.paginate(conn, self.per_page);

        let (items_result, total_result) = tokio::join!(
            paginator.fetch_page(self.page - 1),
            count_paginator.num_items(),
        );

        let items = items_result.map_err(DbError)?;
        let total = total_result.map_err(DbError)?;
        let total_pages = if self.per_page == 0 {
            0
        } else {
            total.div_ceil(self.per_page)
        };

        Ok(Paginated {
            data: items,
            page: self.page,
            per_page: self.per_page,
            total,
            total_pages,
            has_prev: self.page > 1,
            has_next: self.page < total_pages,
        })
    }
}

/// Paginated response wrapper. Implements `IntoResponse` so it can be
/// returned directly from handlers without `Json<>` wrapping.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Paginated<T> {
    pub data: Vec<T>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
    pub total_pages: u64,
    pub has_prev: bool,
    pub has_next: bool,
}

impl<T> Paginated<T> {
    /// Transforms the data items while preserving pagination metadata.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> Paginated<U> {
        Paginated {
            data: self.data.into_iter().map(f).collect(),
            page: self.page,
            per_page: self.per_page,
            total: self.total,
            total_pages: self.total_pages,
            has_prev: self.has_prev,
            has_next: self.has_next,
        }
    }
}

impl<T: Serialize> IntoResponse for Paginated<T> {
    fn into_response(self) -> http::Response<BoxBody> {
        let body = serde_json::to_vec(&self).unwrap_or_default();
        http::Response::builder()
            .status(http::StatusCode::OK)
            .header("content-type", "application/json")
            .body(crate::response::full(body))
            .unwrap()
    }
}

// ── Cursor-based pagination ────────────────────────────────────────────────

/// Raw query params for cursor pagination. All optional so the no-args
/// case yields the first page at the default limit.
#[derive(Deserialize)]
struct CursorPaginateQuery {
    after: Option<String>,
    before: Option<String>,
    limit: Option<u64>,
}

fn encode_cursor<V: Serialize>(value: &V) -> Result<String, Error> {
    let json = serde_json::to_vec(value)
        .map_err(|e| Error::validation(format!("failed to encode cursor: {e}")))?;
    Ok(BASE64.encode(json))
}

fn decode_cursor<V: for<'de> Deserialize<'de>>(token: &str) -> Result<V, Error> {
    let bytes = BASE64
        .decode(token)
        .map_err(|e| Error::validation(format!("invalid cursor: {e}")))?;
    serde_json::from_slice(&bytes).map_err(|e| Error::validation(format!("invalid cursor: {e}")))
}

/// Direction of cursor traversal. Set by which token is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorDirection {
    /// First page, or `?after=` continuation.
    Forward,
    /// `?before=` reverse continuation.
    Backward,
}

/// Cursor-based pagination extractor. Reads `?after=<token>&before=<token>&limit=<n>`.
///
/// The type parameter `V` is the typed cursor value the after/before tokens
/// decode into. It must match the column you call `cursor_by` on in
/// [`CursorPaginate::exec`]. For example, `CursorPaginate<i32>` for an `id`
/// column, or `CursorPaginate<chrono::DateTime<Utc>>` for `created_at`.
///
/// Returns 422 when:
/// - both `after` and `before` are supplied
/// - `limit` is zero or exceeds the configured max
/// - a token fails to base64-decode or fails to deserialize as `V`
#[derive(Debug, Clone)]
pub struct CursorPaginate<V> {
    /// Decoded `after` cursor, when present.
    pub after: Option<V>,
    /// Decoded `before` cursor, when present.
    pub before: Option<V>,
    /// Number of items requested. Falls back to config default.
    pub limit: u64,
    direction: CursorDirection,
}

impl<V> FromRequestParts for CursorPaginate<V>
where
    V: for<'de> Deserialize<'de> + Send + Sync + 'static,
{
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let query_str = parts.uri.query().unwrap_or("");
        let raw: CursorPaginateQuery = serde_urlencoded::from_str(query_str)
            .map_err(|e| Error::validation(format!("invalid cursor params: {}", e)))?;

        let config = state.get::<PaginationConfig>();
        let default_limit = config.map_or(DEFAULT_PER_PAGE, |c| c.default_per_page);
        let max_limit = config.map_or(DEFAULT_MAX_PER_PAGE, |c| c.max_per_page);

        if raw.after.is_some() && raw.before.is_some() {
            return Err(Error::validation(
                "after and before cannot be combined; use one or the other",
            ));
        }

        let limit = raw.limit.unwrap_or(default_limit);
        if limit == 0 {
            return Err(Error::validation("limit must be >= 1"));
        }
        if limit > max_limit {
            return Err(Error::validation(format!("limit must be <= {max_limit}")));
        }

        let after = raw.after.as_deref().map(decode_cursor::<V>).transpose()?;
        let before = raw.before.as_deref().map(decode_cursor::<V>).transpose()?;

        let direction = if before.is_some() {
            CursorDirection::Backward
        } else {
            CursorDirection::Forward
        };

        Ok(CursorPaginate {
            after,
            before,
            limit,
            direction,
        })
    }
}

/// Cursor-paginated response wrapper. Implements `IntoResponse` so it can be
/// returned directly from handlers.
///
/// The token strings in `next_cursor` and `prev_cursor` are opaque base64
/// payloads. Clients should not parse them; they should round-trip them
/// verbatim into the next request's `?after=` or `?before=`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct CursorPaginated<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
    pub prev_cursor: Option<String>,
}

impl<T> CursorPaginated<T> {
    /// Transforms data items while preserving cursor metadata.
    pub fn map<U>(self, f: impl FnMut(T) -> U) -> CursorPaginated<U> {
        CursorPaginated {
            data: self.data.into_iter().map(f).collect(),
            next_cursor: self.next_cursor,
            prev_cursor: self.prev_cursor,
        }
    }
}

impl<T: Serialize> IntoResponse for CursorPaginated<T> {
    fn into_response(self) -> http::Response<BoxBody> {
        let body = serde_json::to_vec(&self).unwrap_or_default();
        http::Response::builder()
            .status(http::StatusCode::OK)
            .header("content-type", "application/json")
            .body(crate::response::full(body))
            .unwrap()
    }
}

/// Declares the default cursor column for a SeaORM model.
///
/// Implementing this trait once per entity lets handlers call
/// [`CursorPaginate::exec`] with just the query and connection. Without it,
/// reach for [`CursorPaginate::exec_by`] and pass the column and accessor
/// at the call site.
///
/// ```rust,ignore
/// impl CursorKey for user::Model {
///     type Column = user::Column;
///     type Value = i32;
///     const COLUMN: Self::Column = user::Column::Id;
///     fn cursor_value(&self) -> Self::Value { self.id }
/// }
/// ```
pub trait CursorKey {
    /// The SeaORM column enum for the entity.
    type Column: ColumnTrait;
    /// The cursor value type. Must match the column's runtime type.
    type Value: Serialize + IntoValueTuple;

    /// The column to sort and seek on.
    const COLUMN: Self::Column;

    /// Reads the cursor value off a row.
    fn cursor_value(&self) -> Self::Value;
}

impl<V: Serialize> CursorPaginate<V> {
    /// Runs a cursor-paginated query against an entity whose default cursor
    /// is declared via [`CursorKey`]. For one-off cursors on a non-default
    /// column, use [`CursorPaginate::exec_by`].
    pub async fn exec<E>(
        self,
        select: Select<E>,
        conn: &sea_orm::DatabaseConnection,
    ) -> Result<CursorPaginated<E::Model>, Error>
    where
        E: EntityTrait,
        E::Model: CursorKey<Value = V> + Send + Sync,
        V: IntoValueTuple,
    {
        self.exec_by(
            select,
            <E::Model as CursorKey>::COLUMN,
            conn,
            <E::Model as CursorKey>::cursor_value,
        )
        .await
    }

    /// Runs a cursor-paginated query, specifying the column and value
    /// accessor explicitly. Use this when the cursor is on a non-default
    /// column or when no [`CursorKey`] impl exists for the entity.
    ///
    /// `extract` must return the same column value that `column` selects on;
    /// otherwise the next and previous tokens will not point where the client
    /// expects.
    pub async fn exec_by<E, C, F>(
        self,
        select: Select<E>,
        column: C,
        conn: &sea_orm::DatabaseConnection,
        extract: F,
    ) -> Result<CursorPaginated<E::Model>, Error>
    where
        E: EntityTrait,
        E::Model: Send + Sync,
        C: ColumnTrait,
        V: IntoValueTuple,
        F: Fn(&E::Model) -> V,
    {
        let CursorPaginate {
            after,
            before,
            limit,
            direction,
        } = self;

        let mut cursor = select.cursor_by(column);
        let had_after = after.is_some();
        let had_before = before.is_some();
        if let Some(v) = after {
            cursor.after(v);
        }
        if let Some(v) = before {
            cursor.before(v);
        }

        let take = limit + 1;
        let rows: Vec<E::Model> = match direction {
            CursorDirection::Forward => cursor.first(take).all(conn).await,
            CursorDirection::Backward => cursor.last(take).all(conn).await,
        }
        .map_err(DbError)?;

        let mut data = rows;
        let has_more = data.len() as u64 > limit;
        if has_more {
            // Trim the overflow row from whichever end is further from the
            // cursor; the kept page is the `limit` rows closest to it.
            match direction {
                CursorDirection::Forward => data.truncate(limit as usize),
                CursorDirection::Backward => {
                    data.drain(..(data.len() - limit as usize));
                }
            }
        }

        let token = |row: Option<&E::Model>| -> Result<Option<String>, Error> {
            row.map(&extract).as_ref().map(encode_cursor).transpose()
        };

        let (next_cursor, prev_cursor) = match direction {
            CursorDirection::Forward => (
                if has_more { token(data.last())? } else { None },
                if had_after {
                    token(data.first())?
                } else {
                    None
                },
            ),
            CursorDirection::Backward => (
                if had_before {
                    token(data.last())?
                } else {
                    None
                },
                if has_more { token(data.first())? } else { None },
            ),
        };

        Ok(CursorPaginated {
            data,
            next_cursor,
            prev_cursor,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{TestRequest, empty_params, empty_state, state_with};

    #[tokio::test]
    async fn test_defaults_no_query_params() {
        let (parts, _) = TestRequest::get("/users").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let p = result.unwrap();
        assert_eq!(p.page, 1);
        assert_eq!(p.per_page, 20);
    }

    #[tokio::test]
    async fn test_explicit_params() {
        let (parts, _) = TestRequest::get("/users?page=3&per_page=50").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let p = result.unwrap();
        assert_eq!(p.page, 3);
        assert_eq!(p.per_page, 50);
    }

    #[tokio::test]
    async fn test_page_zero_rejected() {
        let (parts, _) = TestRequest::get("/users?page=0").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("page must be >= 1"));
    }

    #[tokio::test]
    async fn test_per_page_zero_rejected() {
        let (parts, _) = TestRequest::get("/users?per_page=0").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("per_page must be >= 1"));
    }

    #[tokio::test]
    async fn test_per_page_exceeds_max_rejected() {
        let (parts, _) = TestRequest::get("/users?per_page=101").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("per_page must be <= 100"));
    }

    #[tokio::test]
    async fn test_custom_config_defaults() {
        let state = state_with(PaginationConfig {
            default_per_page: 25,
            max_per_page: 50,
        });
        let (parts, _) = TestRequest::get("/users").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &state).await;

        let p = result.unwrap();
        assert_eq!(p.per_page, 25);
    }

    #[tokio::test]
    async fn test_custom_config_max_enforced() {
        let state = state_with(PaginationConfig {
            default_per_page: 25,
            max_per_page: 50,
        });
        let (parts, _) = TestRequest::get("/users?per_page=51").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &state).await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("per_page must be <= 50"));
    }

    #[tokio::test]
    async fn test_paginated_response_shape() {
        let paginated = Paginated {
            data: vec!["a", "b", "c"],
            page: 2,
            per_page: 10,
            total: 25,
            total_pages: 3,
            has_prev: true,
            has_next: true,
        };

        let response = paginated.into_response();
        assert_eq!(response.status(), http::StatusCode::OK);
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "application/json"
        );

        use http_body_util::BodyExt;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["data"], serde_json::json!(["a", "b", "c"]));
        assert_eq!(json["page"], 2);
        assert_eq!(json["per_page"], 10);
        assert_eq!(json["total"], 25);
        assert_eq!(json["total_pages"], 3);
        assert_eq!(json["has_prev"], true);
        assert_eq!(json["has_next"], true);
    }

    #[test]
    fn test_paginated_first_page_flags() {
        let p: Paginated<String> = Paginated {
            data: vec![],
            page: 1,
            per_page: 10,
            total: 30,
            total_pages: 3,
            has_prev: false,
            has_next: true,
        };
        assert!(!p.has_prev);
        assert!(p.has_next);
    }

    #[test]
    fn test_paginated_last_page_flags() {
        let p: Paginated<String> = Paginated {
            data: vec![],
            page: 3,
            per_page: 10,
            total: 30,
            total_pages: 3,
            has_prev: true,
            has_next: false,
        };
        assert!(p.has_prev);
        assert!(!p.has_next);
    }

    #[test]
    fn test_paginated_single_page() {
        let p: Paginated<String> = Paginated {
            data: vec![],
            page: 1,
            per_page: 10,
            total: 5,
            total_pages: 1,
            has_prev: false,
            has_next: false,
        };
        assert!(!p.has_prev);
        assert!(!p.has_next);
    }

    #[test]
    fn test_pagination_config_default() {
        let config = PaginationConfig::default();
        assert_eq!(config.default_per_page, 20);
        assert_eq!(config.max_per_page, 100);
    }

    #[test]
    fn test_map_transforms_data() {
        let p = Paginated {
            data: vec![1, 2, 3],
            page: 1,
            per_page: 10,
            total: 3,
            total_pages: 1,
            has_prev: false,
            has_next: false,
        };

        let mapped = p.map(|n| n * 2);
        assert_eq!(mapped.data, vec![2, 4, 6]);
        assert_eq!(mapped.page, 1);
        assert_eq!(mapped.total, 3);
    }

    #[test]
    fn test_map_changes_type() {
        let p = Paginated {
            data: vec![1, 2],
            page: 2,
            per_page: 10,
            total: 12,
            total_pages: 2,
            has_prev: true,
            has_next: false,
        };

        let mapped = p.map(|n| format!("item-{}", n));
        assert_eq!(mapped.data, vec!["item-1", "item-2"]);
        assert_eq!(mapped.page, 2);
        assert_eq!(mapped.total_pages, 2);
        assert!(mapped.has_prev);
        assert!(!mapped.has_next);
    }

    #[tokio::test]
    async fn test_non_numeric_page_rejected() {
        let (parts, _) = TestRequest::get("/users?page=abc").into_parts();
        let result = Paginate::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let err = result.unwrap_err();
        assert_eq!(err.status(), 422);
    }

    // ── Cursor codec ──────────────────────────────────────────────────

    #[test]
    fn test_cursor_codec_roundtrip_i32() {
        let token = encode_cursor(&42i32).unwrap();
        let decoded: i32 = decode_cursor(&token).unwrap();
        assert_eq!(decoded, 42);
    }

    #[test]
    fn test_cursor_codec_roundtrip_string() {
        let token = encode_cursor(&"hello world".to_string()).unwrap();
        let decoded: String = decode_cursor(&token).unwrap();
        assert_eq!(decoded, "hello world");
    }

    #[test]
    fn test_cursor_codec_token_is_url_safe_base64_of_json() {
        // Lock the wire format: URL-safe unpadded base64 of the JSON bytes.
        // Documented in pagination.md; integration tests rely on this shape.
        let token = encode_cursor(&7i32).unwrap();
        assert!(!token.contains('+') && !token.contains('/') && !token.contains('='));
        let bytes = BASE64.decode(&token).unwrap();
        assert_eq!(bytes, b"7");
    }

    #[test]
    fn test_cursor_decode_rejects_non_base64() {
        let err = decode_cursor::<i32>("not!!!base64").unwrap_err();
        assert_eq!(err.status(), 422);
    }

    #[test]
    fn test_cursor_decode_rejects_wrong_type() {
        let token = encode_cursor(&42i32).unwrap();
        let err = decode_cursor::<String>(&token).unwrap_err();
        assert_eq!(err.status(), 422);
    }

    // ── CursorPaginate extractor ──────────────────────────────────────

    #[tokio::test]
    async fn test_cursor_paginate_defaults() {
        let (parts, _) = TestRequest::get("/items").into_parts();
        let p: CursorPaginate<i32> =
            CursorPaginate::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap();
        assert!(p.after.is_none());
        assert!(p.before.is_none());
        assert_eq!(p.limit, 20);
        assert_eq!(p.direction, CursorDirection::Forward);
    }

    #[tokio::test]
    async fn test_cursor_paginate_after_token() {
        let token = encode_cursor(&100i32).unwrap();
        let (parts, _) = TestRequest::get(&format!("/items?after={}", token)).into_parts();
        let p: CursorPaginate<i32> =
            CursorPaginate::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap();
        assert_eq!(p.after, Some(100));
        assert_eq!(p.direction, CursorDirection::Forward);
    }

    #[tokio::test]
    async fn test_cursor_paginate_before_token() {
        let token = encode_cursor(&50i32).unwrap();
        let (parts, _) = TestRequest::get(&format!("/items?before={}", token)).into_parts();
        let p: CursorPaginate<i32> =
            CursorPaginate::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap();
        assert_eq!(p.before, Some(50));
        assert_eq!(p.direction, CursorDirection::Backward);
    }

    #[tokio::test]
    async fn test_cursor_paginate_after_and_before_rejected() {
        let after = encode_cursor(&1i32).unwrap();
        let before = encode_cursor(&10i32).unwrap();
        let (parts, _) =
            TestRequest::get(&format!("/items?after={}&before={}", after, before)).into_parts();
        let err =
            CursorPaginate::<i32>::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("after and before"));
    }

    #[tokio::test]
    async fn test_cursor_paginate_limit_zero_rejected() {
        let (parts, _) = TestRequest::get("/items?limit=0").into_parts();
        let err =
            CursorPaginate::<i32>::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap_err();
        assert_eq!(err.status(), 422);
    }

    #[tokio::test]
    async fn test_cursor_paginate_limit_exceeds_max_rejected() {
        let (parts, _) = TestRequest::get("/items?limit=101").into_parts();
        let err =
            CursorPaginate::<i32>::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap_err();
        assert_eq!(err.status(), 422);
        assert!(err.message().contains("<= 100"));
    }

    #[tokio::test]
    async fn test_cursor_paginate_garbage_token_rejected() {
        let (parts, _) = TestRequest::get("/items?after=!!!").into_parts();
        let err =
            CursorPaginate::<i32>::from_request_parts(&parts, &empty_params(), &empty_state())
                .await
                .unwrap_err();
        assert_eq!(err.status(), 422);
    }

    #[tokio::test]
    async fn test_cursor_paginated_response_shape() {
        let p = CursorPaginated {
            data: vec!["a", "b", "c"],
            next_cursor: Some("opaque-next".to_string()),
            prev_cursor: None,
        };

        let response = p.into_response();
        assert_eq!(response.status(), http::StatusCode::OK);
        use http_body_util::BodyExt;
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["data"], serde_json::json!(["a", "b", "c"]));
        assert_eq!(json["next_cursor"], "opaque-next");
        assert!(json["prev_cursor"].is_null());
        // Lock the schema: no total, no has_more.
        assert!(json.get("total").is_none());
        assert!(json.get("has_more").is_none());
    }

    #[test]
    fn test_cursor_paginated_map() {
        let p = CursorPaginated {
            data: vec![1, 2, 3],
            next_cursor: Some("n".into()),
            prev_cursor: Some("p".into()),
        };
        let mapped = p.map(|n| n * 10);
        assert_eq!(mapped.data, vec![10, 20, 30]);
        assert_eq!(mapped.next_cursor.as_deref(), Some("n"));
        assert_eq!(mapped.prev_cursor.as_deref(), Some("p"));
    }
}
