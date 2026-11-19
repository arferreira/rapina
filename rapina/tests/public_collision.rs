//! Publicness must belong to a route, not to a handler name.
//!
//! Lives in its own test binary so these same-named handlers don't share an
//! `inventory` collection with other test files.

use http::StatusCode;
use rapina::prelude::*;
use rapina::testing::TestClient;

mod partner {
    use rapina::prelude::*;

    #[public]
    #[get("/partner/things")]
    pub async fn things_route() -> &'static str {
        "partner"
    }

    #[public]
    #[get("/partner/stats")]
    pub async fn stats_route() -> &'static str {
        "partner stats"
    }
}

mod admin {
    use rapina::prelude::*;

    #[get("/admin/things")]
    pub async fn things_route(user: CurrentUser) -> String {
        format!("admin {}", user.id)
    }

    #[get("/admin/stats")]
    pub async fn stats_route() -> &'static str {
        "admin stats"
    }
}

#[rapina::get("/qualified-below")]
#[rapina::public]
async fn qualified_below() -> &'static str {
    "qualified below"
}

const SECRET: &str = "test-secret-public-collision";

async fn client() -> TestClient {
    let app = Rapina::new()
        .with_introspection(false)
        .with_auth(AuthConfig::new(SECRET, 3600))
        .discover();
    TestClient::new(app).await
}

#[tokio::test]
async fn public_twin_stays_public() {
    let client = client().await;

    let resp = client.get("/partner/things").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "partner");

    let resp = client.get("/partner/stats").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn protected_twin_requires_token() {
    let client = client().await;

    let resp = client.get("/admin/things").send().await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

    let resp = client.get("/admin/stats").send().await;
    assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn protected_twin_accepts_valid_token() {
    let client = client().await;
    let token = AuthConfig::new(SECRET, 3600).create_token("u1").unwrap();

    let resp = client
        .get("/admin/things")
        .header("authorization", &format!("Bearer {token}"))
        .send()
        .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(resp.text(), "admin u1");
}

#[tokio::test]
async fn qualified_public_below_route_macro_is_public() {
    let client = client().await;

    let resp = client.get("/qualified-below").send().await;
    assert_eq!(resp.status(), StatusCode::OK);
}
