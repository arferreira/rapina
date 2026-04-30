//! Endpoint for serving llms.txt from the running application.

use std::sync::Arc;

use http::{Request, Response, StatusCode, header::CONTENT_TYPE};
use hyper::body::Incoming;

use crate::extract::PathParams;
use crate::response::{BoxBody, IntoResponse, full};
use crate::state::AppState;

/// Stores the pre-generated llms.txt content.
///
/// Generated once at startup in [`crate::app::Rapina::prepare`] and shared
/// across requests via [`Arc`] inside [`AppState`].
#[derive(Debug, Clone)]
pub struct LlmsRegistry {
    content: Arc<String>,
}

impl LlmsRegistry {
    /// Create a registry holding the given pre-rendered content.
    pub fn new(content: String) -> Self {
        Self {
            content: Arc::new(content),
        }
    }

    /// Returns the pre-rendered llms.txt content.
    pub fn content(&self) -> &str {
        &self.content
    }
}

/// Handler for `GET /__rapina/llms.txt`.
///
/// Returns the pre-generated Markdown document as `text/plain; charset=utf-8`.
pub async fn llms_txt_handler(
    _req: Request<Incoming>,
    _params: PathParams,
    state: Arc<AppState>,
) -> Response<BoxBody> {
    let registry = state.get::<LlmsRegistry>();

    match registry {
        Some(registry) => {
            let body = bytes::Bytes::from(registry.content().to_owned());
            Response::builder()
                .status(StatusCode::OK)
                .header(CONTENT_TYPE, "text/plain; charset=utf-8")
                .body(full(body))
                .unwrap()
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
