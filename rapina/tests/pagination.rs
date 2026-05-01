//! Integration tests for the pagination module.

#![cfg(feature = "database")]

use http::StatusCode;
use rapina::pagination::{Paginate, PaginationConfig};
use rapina::prelude::*;
use rapina::testing::TestClient;
use std::sync::Arc;

// -- Paginate extractor via TestClient --

#[tokio::test]
async fn test_paginate_defaults_via_handler() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(Router::new().route(
            http::Method::GET,
            "/items",
            |req, params, state: Arc<rapina::state::AppState>| async move {
                let (parts, _) = req.into_parts();
                let p = Paginate::from_request_parts(&parts, &params, &state)
                    .await
                    .unwrap();
                Json(serde_json::json!({
                    "page": p.page,
                    "per_page": p.per_page,
                }))
            },
        ));

    let client = TestClient::new(app).await;
    let response = client.get("/items").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = response.json();
    assert_eq!(json["page"], 1);
    assert_eq!(json["per_page"], 20);
}

#[tokio::test]
async fn test_paginate_explicit_params_via_handler() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(Router::new().route(
            http::Method::GET,
            "/items",
            |req, params, state: Arc<rapina::state::AppState>| async move {
                let (parts, _) = req.into_parts();
                let p = Paginate::from_request_parts(&parts, &params, &state)
                    .await
                    .unwrap();
                Json(serde_json::json!({
                    "page": p.page,
                    "per_page": p.per_page,
                }))
            },
        ));

    let client = TestClient::new(app).await;
    let response = client.get("/items?page=3&per_page=50").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = response.json();
    assert_eq!(json["page"], 3);
    assert_eq!(json["per_page"], 50);
}

#[tokio::test]
async fn test_paginate_invalid_page_returns_422() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(Router::new().route(
            http::Method::GET,
            "/items",
            |req, params, state: Arc<rapina::state::AppState>| async move {
                let (parts, _) = req.into_parts();
                match Paginate::from_request_parts(&parts, &params, &state).await {
                    Ok(p) => Json(serde_json::json!({"page": p.page})).into_response(),
                    Err(e) => e.into_response(),
                }
            },
        ));

    let client = TestClient::new(app).await;
    let response = client.get("/items?page=0").send().await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_paginate_per_page_exceeds_max_returns_422() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(Router::new().route(
            http::Method::GET,
            "/items",
            |req, params, state: Arc<rapina::state::AppState>| async move {
                let (parts, _) = req.into_parts();
                match Paginate::from_request_parts(&parts, &params, &state).await {
                    Ok(p) => Json(serde_json::json!({"per_page": p.per_page})).into_response(),
                    Err(e) => e.into_response(),
                }
            },
        ));

    let client = TestClient::new(app).await;
    let response = client.get("/items?per_page=101").send().await;

    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn test_paginate_respects_custom_config() {
    let app = Rapina::new()
        .with_introspection(false)
        .state(PaginationConfig {
            default_per_page: 25,
            max_per_page: 50,
        })
        .router(Router::new().route(
            http::Method::GET,
            "/items",
            |req, params, state: Arc<rapina::state::AppState>| async move {
                let (parts, _) = req.into_parts();
                match Paginate::from_request_parts(&parts, &params, &state).await {
                    Ok(p) => Json(serde_json::json!({"page": p.page, "per_page": p.per_page}))
                        .into_response(),
                    Err(e) => e.into_response(),
                }
            },
        ));

    let client = TestClient::new(app).await;

    // Default per_page should come from config
    let response = client.get("/items").send().await;
    assert_eq!(response.status(), StatusCode::OK);
    let json: serde_json::Value = response.json();
    assert_eq!(json["per_page"], 25);

    // Exceeding custom max returns 422
    let response = client.get("/items?per_page=51").send().await;
    assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

    // Within custom max is fine
    let response = client.get("/items?per_page=50").send().await;
    assert_eq!(response.status(), StatusCode::OK);
}

// -- Paginated<T> response --

use rapina::pagination::Paginated;

#[tokio::test]
async fn test_paginated_response_via_handler() {
    let app = Rapina::new()
        .with_introspection(false)
        .router(
            Router::new().route(http::Method::GET, "/items", |_, _, _| async move {
                Paginated {
                    data: vec!["a", "b", "c"],
                    page: 2,
                    per_page: 3,
                    total: 9,
                    total_pages: 3,
                    has_prev: true,
                    has_next: true,
                }
            }),
        );

    let client = TestClient::new(app).await;
    let response = client.get("/items").send().await;

    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        response
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("application/json")
    );

    let json: serde_json::Value = response.json();
    assert_eq!(json["data"], serde_json::json!(["a", "b", "c"]));
    assert_eq!(json["page"], 2);
    assert_eq!(json["per_page"], 3);
    assert_eq!(json["total"], 9);
    assert_eq!(json["total_pages"], 3);
    assert_eq!(json["has_prev"], true);
    assert_eq!(json["has_next"], true);
}

use rapina::extract::FromRequestParts;

// ── Cursor pagination end-to-end (sqlite-backed) ──────────────────────────

#[cfg(feature = "sqlite")]
mod cursor_e2e {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use rapina::pagination::CursorPaginate;
    use rapina::sea_orm::{
        ActiveModelTrait, ConnectionTrait, Database, DatabaseConnection, DbBackend, EntityTrait,
        Schema, Set,
    };

    mod entity {
        use rapina::pagination::CursorKey;
        use rapina::sea_orm::entity::prelude::*;

        #[derive(Clone, Debug, PartialEq, DeriveEntityModel, serde::Serialize)]
        #[sea_orm(table_name = "items")]
        pub struct Model {
            #[sea_orm(primary_key)]
            pub id: i32,
            pub name: String,
        }

        #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
        pub enum Relation {}

        impl ActiveModelBehavior for ActiveModel {}

        impl CursorKey for Model {
            type Column = Column;
            type Value = i32;
            const COLUMN: Column = Column::Id;
            fn cursor_value(&self) -> i32 {
                self.id
            }
        }
    }

    /// Mint a cursor token directly. The wire format is `URL_SAFE_NO_PAD(json(value))`,
    /// documented in `pagination.md` and locked by the unit tests in `pagination.rs`.
    fn mint_token<V: serde::Serialize>(value: &V) -> String {
        URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    }

    async fn setup_db_with(rows: u32) -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:").await.unwrap();
        let backend = conn.get_database_backend();
        let schema = Schema::new(DbBackend::Sqlite);
        let stmt = schema.create_table_from_entity(entity::Entity);
        conn.execute(backend.build(&stmt)).await.unwrap();

        for i in 1..=rows {
            entity::ActiveModel {
                id: Set(i as i32),
                name: Set(format!("item-{i}")),
            }
            .insert(&conn)
            .await
            .unwrap();
        }
        conn
    }

    async fn setup_db() -> DatabaseConnection {
        setup_db_with(10).await
    }

    fn build_app(conn: DatabaseConnection) -> Rapina {
        Rapina::new()
            .with_introspection(false)
            .state(conn)
            .router(Router::new().route(
                http::Method::GET,
                "/items",
                |req, params, state: Arc<rapina::state::AppState>| async move {
                    let (parts, _) = req.into_parts();
                    let cursor =
                        match CursorPaginate::<i32>::from_request_parts(&parts, &params, &state)
                            .await
                        {
                            Ok(c) => c,
                            Err(e) => return e.into_response(),
                        };
                    let conn = state.get::<DatabaseConnection>().unwrap();
                    match cursor.exec(entity::Entity::find(), conn).await {
                        Ok(p) => p.into_response(),
                        Err(e) => e.into_response(),
                    }
                },
            ))
    }

    fn ids(j: &serde_json::Value) -> Vec<i32> {
        j["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_i64().unwrap() as i32)
            .collect()
    }

    #[tokio::test]
    async fn forward_pagination_walks_all_rows() {
        let client = TestClient::new(build_app(setup_db().await)).await;

        let r = client.get("/items?limit=3").send().await;
        assert_eq!(r.status(), StatusCode::OK);
        let j: serde_json::Value = r.json();
        assert_eq!(ids(&j), vec![1, 2, 3]);
        assert!(j["next_cursor"].is_string());
        assert!(j["prev_cursor"].is_null(), "no prev on first page");

        let mut next = j["next_cursor"].as_str().unwrap().to_string();
        let mut all_ids = ids(&j);
        loop {
            let r = client
                .get(&format!("/items?limit=3&after={next}"))
                .send()
                .await;
            assert_eq!(r.status(), StatusCode::OK);
            let j: serde_json::Value = r.json();
            all_ids.extend(ids(&j));
            assert!(j["prev_cursor"].is_string(), "prev set when after was used");
            match j["next_cursor"].as_str() {
                Some(t) => next = t.to_string(),
                None => break,
            }
        }
        assert_eq!(all_ids, (1..=10).collect::<Vec<_>>());
    }

    #[tokio::test]
    async fn last_page_has_no_next_cursor() {
        let client = TestClient::new(build_app(setup_db().await)).await;
        let j: serde_json::Value = client.get("/items?limit=10").send().await.json();
        assert_eq!(ids(&j).len(), 10);
        assert!(j["next_cursor"].is_null());
        assert!(j["prev_cursor"].is_null());
    }

    #[tokio::test]
    async fn empty_table_returns_empty_page() {
        let client = TestClient::new(build_app(setup_db_with(0).await)).await;
        let j: serde_json::Value = client.get("/items?limit=5").send().await.json();
        assert!(ids(&j).is_empty());
        assert!(j["next_cursor"].is_null());
        assert!(j["prev_cursor"].is_null());
    }

    #[tokio::test]
    async fn limit_one_walks_one_row_at_a_time() {
        let client = TestClient::new(build_app(setup_db_with(3).await)).await;

        let j: serde_json::Value = client.get("/items?limit=1").send().await.json();
        assert_eq!(ids(&j), vec![1]);
        let next = j["next_cursor"].as_str().unwrap().to_string();

        let j: serde_json::Value = client
            .get(&format!("/items?limit=1&after={next}"))
            .send()
            .await
            .json();
        assert_eq!(ids(&j), vec![2]);
        let next = j["next_cursor"].as_str().unwrap().to_string();

        let j: serde_json::Value = client
            .get(&format!("/items?limit=1&after={next}"))
            .send()
            .await
            .json();
        assert_eq!(ids(&j), vec![3]);
        assert!(j["next_cursor"].is_null(), "last row, no next");
    }

    #[tokio::test]
    async fn stale_cursor_past_end_returns_empty_page() {
        let client = TestClient::new(build_app(setup_db_with(5).await)).await;
        let token = mint_token(&999i32);
        let j: serde_json::Value = client
            .get(&format!("/items?limit=3&after={token}"))
            .send()
            .await
            .json();
        assert!(ids(&j).is_empty());
        assert!(j["next_cursor"].is_null());
        // No prev_cursor either: there were zero rows to extract from.
        assert!(j["prev_cursor"].is_null());
    }

    #[tokio::test]
    async fn next_cursor_round_trips_to_prev_cursor() {
        let client = TestClient::new(build_app(setup_db().await)).await;

        // Walk forward two pages.
        let j: serde_json::Value = client.get("/items?limit=3").send().await.json();
        assert_eq!(ids(&j), vec![1, 2, 3]);
        let after_p1 = j["next_cursor"].as_str().unwrap().to_string();

        let j: serde_json::Value = client
            .get(&format!("/items?limit=3&after={after_p1}"))
            .send()
            .await
            .json();
        assert_eq!(ids(&j), vec![4, 5, 6]);
        let prev_p2 = j["prev_cursor"].as_str().unwrap().to_string();

        // Use prev_cursor to step back: should land on a page strictly before id=4.
        let j: serde_json::Value = client
            .get(&format!("/items?limit=3&before={prev_p2}"))
            .send()
            .await
            .json();
        assert_eq!(ids(&j), vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn backward_pagination_returns_ascending_data() {
        let client = TestClient::new(build_app(setup_db().await)).await;
        let token = mint_token(&8i32);

        let j: serde_json::Value = client
            .get(&format!("/items?limit=3&before={token}"))
            .send()
            .await
            .json();
        // The three rows immediately behind id=8, ordered ascending.
        assert_eq!(ids(&j), vec![5, 6, 7]);
        assert!(j["next_cursor"].is_string());
        assert!(j["prev_cursor"].is_string());
    }
}
