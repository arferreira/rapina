//! SSE counter example.
//!
//! Run with `cargo run --example sse`, then `curl -N http://127.0.0.1:3000/events`
//! to watch one event arrive per second.

use std::time::Duration;

use rapina::http::Method;
use rapina::prelude::*;
use rapina::response::{BodyError, SseEvent, SseResponse};
use rapina::router::Router;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let router = Router::new().route(Method::GET, "/events", |_, _, _| async {
        let stream = async_stream::stream! {
            for i in 0u64.. {
                tokio::time::sleep(Duration::from_secs(1)).await;
                yield Ok::<_, BodyError>(SseEvent::data(format!("tick {}", i)).id(i.to_string()));
            }
        };
        SseResponse::new(stream)
    });

    Rapina::new()
        .with_health_check(true)
        .router(router)
        .listen("127.0.0.1:3000")
        .await
}
