use std::io::Write;
use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use flate2::Compression;
use flate2::write::{DeflateEncoder, GzEncoder};
use http::{HeaderValue, Response, header};
use http_body_util::BodyExt;
use hyper::Request;
use hyper::body::{Body, Frame, Incoming, SizeHint};
use pin_project_lite::pin_project;

use crate::context::RequestContext;
use crate::response::{APPLICATION_JSON, BodyError, BoxBody, empty, full};

use super::{BoxFuture, Middleware, Next};

const DEFAULT_MIN_SIZE: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
enum Algorithm {
    Gzip,
    Deflate,
}

impl Algorithm {
    fn from_accept_encoding(header: &str) -> Option<Self> {
        if header.contains("gzip") {
            Some(Algorithm::Gzip)
        } else if header.contains("deflate") {
            Some(Algorithm::Deflate)
        } else {
            None
        }
    }

    fn content_encoding(&self) -> &'static str {
        match self {
            Algorithm::Gzip => "gzip",
            Algorithm::Deflate => "deflate",
        }
    }

    fn compress(&self, data: &[u8], level: Compression) -> std::io::Result<Vec<u8>> {
        match self {
            Algorithm::Gzip => {
                let mut encoder = GzEncoder::new(Vec::new(), level);
                encoder.write_all(data)?;
                encoder.finish()
            }
            Algorithm::Deflate => {
                let mut encoder = DeflateEncoder::new(Vec::new(), level);
                encoder.write_all(data)?;
                encoder.finish()
            }
        }
    }
}

/// Configuration for [`CompressionMiddleware`].
#[derive(Debug, Clone)]
pub struct CompressionConfig {
    /// Minimum response body size in bytes required for compression to run.
    /// Responses smaller than this value are sent uncompressed. Defaults to 1024 bytes.
    pub min_size: usize,
    /// Compression level from 0 (none) to 9 (best). Values above 9 are
    /// clamped to 9. Defaults to 6.
    pub level: u32,
}

impl CompressionConfig {
    pub fn new(min_size: usize, level: u32) -> Self {
        Self {
            min_size,
            level: level.min(9),
        }
    }
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            min_size: DEFAULT_MIN_SIZE,
            level: 6,
        }
    }
}

/// Middleware that compresses response bodies using gzip or deflate.
///
/// Negotiates the encoding via the `Accept-Encoding` request header (gzip is
/// preferred over deflate). Only text-based content types such as
/// `application/json` and `text/*` are compressed; binary responses are passed
/// through unchanged. Responses smaller than [`CompressionConfig::min_size`]
/// are also left uncompressed. If compression does not reduce the payload size
/// the original body is returned.
///
/// # Example
///
/// ```rust,ignore
/// Rapina::new()
///     .with(CompressionMiddleware::new(CompressionConfig::new(512, 6)))
/// ```
#[derive(Debug, Clone)]
pub struct CompressionMiddleware {
    config: CompressionConfig,
}

impl CompressionMiddleware {
    pub fn new(config: CompressionConfig) -> Self {
        Self { config }
    }

    fn is_compressible_content_type(content_type: Option<&HeaderValue>) -> bool {
        let Some(ct) = content_type else {
            return true;
        };

        let ct_str = ct.to_str().unwrap_or("");

        ct_str.starts_with("text/")
            || ct_str.starts_with(APPLICATION_JSON)
            || ct_str.starts_with("application/xml")
            || ct_str.starts_with("application/javascript")
            || ct_str.contains("+json")
            || ct_str.contains("+xml")
    }

    fn is_already_encoded(response: &Response<BoxBody>) -> bool {
        response.headers().contains_key(header::CONTENT_ENCODING)
    }
}

impl Default for CompressionMiddleware {
    fn default() -> Self {
        Self::new(CompressionConfig::default())
    }
}

impl Middleware for CompressionMiddleware {
    fn handle<'a>(
        &'a self,
        req: Request<Incoming>,
        _ctx: &'a RequestContext,
        next: Next<'a>,
    ) -> BoxFuture<'a, Response<BoxBody>> {
        Box::pin(async move {
            let accept_encoding = req
                .headers()
                .get(header::ACCEPT_ENCODING)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");

            let algorithm = Algorithm::from_accept_encoding(accept_encoding);

            let response = next.run(req).await;

            // SSE bodies must never be compressed: gzip across event boundaries
            // breaks framing at proxies. See Starlette's GZipMiddleware (commit
            // a9a8dab, Feb 2025) for the same exclusion.
            let is_event_stream = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|s| s.starts_with("text/event-stream"))
                .unwrap_or(false);
            if is_event_stream {
                return response;
            }

            let algorithm = match algorithm {
                Some(alg)
                    if !Self::is_already_encoded(&response)
                        && Self::is_compressible_content_type(
                            response.headers().get(header::CONTENT_TYPE),
                        ) =>
                {
                    alg
                }
                _ => return response,
            };

            // Streaming path: per-chunk compression with persistent encoder
            // state. We commit to compressing because the `min_size` guard
            // and "not worth it" check from the buffered path don't apply,
            // the total size isn't knowable in advance.
            let level = Compression::new(self.config.level);
            if response.body().size_hint().exact().is_none() {
                let (mut parts, body) = response.into_parts();
                let wrapped = StreamingCompressedBody::new(body, algorithm, level).boxed_unsync();
                parts.headers.insert(
                    header::CONTENT_ENCODING,
                    HeaderValue::from_static(algorithm.content_encoding()),
                );
                parts.headers.remove(header::CONTENT_LENGTH);
                parts
                    .headers
                    .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));
                return Response::from_parts(parts, wrapped);
            }

            let (parts, body) = response.into_parts();
            let body_bytes = match body.collect().await {
                Ok(collected) => collected.to_bytes(),
                Err(_) => return Response::from_parts(parts, empty()),
            };

            if body_bytes.len() < self.config.min_size {
                return Response::from_parts(parts, full(body_bytes));
            }

            let compressed = match algorithm.compress(&body_bytes, level) {
                Ok(data) => data,
                Err(_) => return Response::from_parts(parts, full(body_bytes)),
            };

            // not worth it
            if compressed.len() >= body_bytes.len() {
                return Response::from_parts(parts, full(body_bytes));
            }

            let mut response = Response::from_parts(parts, full(Bytes::from(compressed)));
            response.headers_mut().insert(
                header::CONTENT_ENCODING,
                HeaderValue::from_static(algorithm.content_encoding()),
            );
            response.headers_mut().remove(header::CONTENT_LENGTH);
            response
                .headers_mut()
                .insert(header::VARY, HeaderValue::from_static("Accept-Encoding"));

            response
        })
    }
}

pin_project! {
    /// Streaming compressor wrapper. Owns the upstream body and a persistent
    /// encoder; emits compressed chunks as upstream chunks arrive. Per Phase 3
    /// of the streaming plan: large file streams should not be buffered.
    struct StreamingCompressedBody {
        #[pin]
        inner: BoxBody,
        encoder: Option<StreamingEncoder>,
        tail: Option<Bytes>,
        done: bool,
    }
}

impl StreamingCompressedBody {
    fn new(inner: BoxBody, algorithm: Algorithm, level: Compression) -> Self {
        Self {
            inner,
            encoder: Some(StreamingEncoder::new(algorithm, level)),
            tail: None,
            done: false,
        }
    }
}

impl Body for StreamingCompressedBody {
    type Data = Bytes;
    type Error = BodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BodyError>>> {
        let mut this = self.project();

        // Emit any leftover tail from a previous finish() before signalling end.
        if let Some(tail) = this.tail.take() {
            return Poll::Ready(Some(Ok(Frame::data(tail))));
        }
        if *this.done {
            return Poll::Ready(None);
        }

        loop {
            match this.inner.as_mut().poll_frame(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Err(e))) => {
                    tracing::error!("compression upstream body yielded error: {}", e);
                    return Poll::Ready(Some(Err(e)));
                }
                Poll::Ready(Some(Ok(frame))) => {
                    if let Ok(data) = frame.into_data() {
                        let encoder = this
                            .encoder
                            .as_mut()
                            .expect("encoder present until upstream end");
                        if let Err(e) = encoder.write(&data) {
                            tracing::error!("compression encoder write failed: {}", e);
                            return Poll::Ready(Some(Err(Box::new(e))));
                        }
                        let chunk = encoder.drain();
                        if !chunk.is_empty() {
                            return Poll::Ready(Some(Ok(Frame::data(chunk))));
                        }
                        // Encoder buffered internally; pull more from upstream.
                        continue;
                    }
                    // Trailers/non-data frames: pass through.
                    // (BoxBody from rapina never carries trailers today.)
                    continue;
                }
                Poll::Ready(None) => {
                    let encoder = match this.encoder.take() {
                        Some(e) => e,
                        None => {
                            *this.done = true;
                            return Poll::Ready(None);
                        }
                    };
                    let tail = match encoder.finish() {
                        Ok(bytes) => Bytes::from(bytes),
                        Err(e) => {
                            tracing::error!("compression encoder finish failed: {}", e);
                            return Poll::Ready(Some(Err(Box::new(e))));
                        }
                    };
                    *this.done = true;
                    if tail.is_empty() {
                        return Poll::Ready(None);
                    }
                    return Poll::Ready(Some(Ok(Frame::data(tail))));
                }
            }
        }
    }

    fn size_hint(&self) -> SizeHint {
        SizeHint::default()
    }
}

enum StreamingEncoder {
    Gzip(GzEncoder<Vec<u8>>),
    Deflate(DeflateEncoder<Vec<u8>>),
}

impl StreamingEncoder {
    fn new(alg: Algorithm, level: Compression) -> Self {
        match alg {
            Algorithm::Gzip => Self::Gzip(GzEncoder::new(Vec::new(), level)),
            Algorithm::Deflate => Self::Deflate(DeflateEncoder::new(Vec::new(), level)),
        }
    }

    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Gzip(e) => e.write_all(data),
            Self::Deflate(e) => e.write_all(data),
        }
    }

    /// Take whatever output bytes the encoder has buffered so far. Empty if
    /// the encoder is still accumulating internally. Reuses the buffer's
    /// capacity across drains so long streams don't reallocate per chunk.
    fn drain(&mut self) -> Bytes {
        let buf = match self {
            Self::Gzip(e) => e.get_mut(),
            Self::Deflate(e) => e.get_mut(),
        };
        let cap = buf.capacity();
        Bytes::from(std::mem::replace(buf, Vec::with_capacity(cap)))
    }

    fn finish(self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Gzip(e) => e.finish(),
            Self::Deflate(e) => e.finish(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = CompressionConfig::default();
        assert_eq!(config.min_size, 1024);
        assert_eq!(config.level, 6);
    }

    #[test]
    fn test_config_clamps_level() {
        let config = CompressionConfig::new(1024, 15);
        assert_eq!(config.level, 9);
    }

    #[test]
    fn test_algorithm_from_accept_encoding() {
        assert_eq!(
            Algorithm::from_accept_encoding("gzip, deflate"),
            Some(Algorithm::Gzip)
        );
        assert_eq!(
            Algorithm::from_accept_encoding("deflate"),
            Some(Algorithm::Deflate)
        );
        assert_eq!(Algorithm::from_accept_encoding("br"), None);
    }

    #[test]
    fn test_gzip_compression() {
        let data = "hello from rapina ".repeat(100);
        let compressed = Algorithm::Gzip
            .compress(data.as_bytes(), Compression::default())
            .unwrap();
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_deflate_compression() {
        let data = "hello from rapina ".repeat(100);
        let compressed = Algorithm::Deflate
            .compress(data.as_bytes(), Compression::default())
            .unwrap();
        assert!(compressed.len() < data.len());
    }

    #[test]
    fn test_is_compressible_content_type() {
        assert!(CompressionMiddleware::is_compressible_content_type(Some(
            &HeaderValue::from_static("text/html")
        )));
        assert!(CompressionMiddleware::is_compressible_content_type(Some(
            &HeaderValue::from_static("application/json")
        )));
        assert!(!CompressionMiddleware::is_compressible_content_type(Some(
            &HeaderValue::from_static("image/png")
        )));
        assert!(CompressionMiddleware::is_compressible_content_type(None));
    }
}
