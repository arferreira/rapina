//! Built-in pagination for database-backed list endpoints.
//!
//! Provides a [`Paginate`] extractor that reads `?page=1&per_page=20` from query
//! params, and a [`Paginated<T>`] response wrapper that serializes data with
//! pagination metadata. The [`Paginate::exec`] method glues them together by
//! running fetch + count concurrently against a SeaORM `Select`.
//!
//! # Quick Start
//!
//! ```rust,ignore
//! use rapina::prelude::*;
//! use rapina::database::Db;
//! use rapina::pagination::{Paginate, Paginated};
//! use entity::user::{self, Entity as User};
//!
//! #[get("/users")]
//! async fn list_users(db: Db, page: Paginate) -> Result<Paginated<user::Model>> {
//!     page.exec(User::find(), db.conn()).await
//! }
//! ```
//!
//! # Configuration
//!
//! Register [`PaginationConfig`] via `.state()` to override defaults:
//!
//! ```rust,ignore
//! use rapina::pagination::PaginationConfig;
//!
//! Rapina::new()
//!     .state(PaginationConfig {
//!         default_per_page: 25,
//!         max_per_page: 50,
//!     })
//!     // ...
//! ```

use std::sync::Arc;

use schemars::JsonSchema;
use sea_orm::{EntityTrait, PaginatorTrait, Select};
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
}
