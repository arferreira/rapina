//! Request extractors for parsing incoming HTTP requests.
//!
//! Extractors are types that implement [`FromRequest`] or [`FromRequestParts`]
//! and can be used as handler parameters to automatically parse request data.

use http::Request;
use http_body_util::BodyExt;
use hyper::body::Incoming;
use serde::de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use smallvec::SmallVec;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use validator::Validate;

#[cfg(feature = "multipart")]
pub mod multipart;
#[cfg(feature = "multipart")]
pub use multipart::{Field, Multipart};

use crate::context::RequestContext;
use crate::error::Error;
use crate::response::{APPLICATION_JSON, BoxBody, FORM_CONTENT_TYPE, IntoResponse};
use crate::state::AppState;
use http::header::CONTENT_TYPE;

/// Extracts and deserializes JSON request bodies.
///
/// Parses the request body as JSON into the specified type `T`.
/// Returns 400 Bad Request if parsing fails.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[derive(Deserialize)]
/// struct CreateUser {
///     name: String,
///     email: String,
/// }
///
/// #[post("/users")]
/// async fn create_user(body: Json<CreateUser>) -> Json<User> {
///     // Use body.name, body.email...
/// }
/// ```
#[derive(Debug)]
pub struct Json<T>(pub T);

/// Extracts path parameters from the URL.
///
/// Supports three extraction modes:
///
/// **Single parameter** — parses one `:param` into any type that implements `FromStr`:
/// ```ignore
/// #[get("/users/:id")]
/// async fn get_user(id: Path<u64>) -> String {
///     format!("User ID: {}", *id)
/// }
/// ```
///
/// **Tuple** — extracts multiple parameters in declaration order (left to right in the pattern):
/// ```ignore
/// #[get("/orgs/:org_id/teams/:team_id")]
/// async fn get_team(Path((org_id, team_id)): Path<(u64, u64)>) -> String {
///     format!("org={} team={}", org_id, team_id)
/// }
/// ```
///
/// **Three or more** — same tuple syntax:
/// ```ignore
/// #[get("/orgs/:org_id/teams/:team_id/members/:member_id")]
/// async fn get_member(Path((org_id, team_id, member_id)): Path<(u64, u64, u64)>) -> String {
///     format!("org={} team={} member={}", org_id, team_id, member_id)
/// }
/// ```
///
/// Returns 400 Bad Request if a parameter is missing or cannot be parsed.
#[derive(Debug)]
pub struct Path<T>(pub T);

/// Extracts and deserializes query string parameters.
///
/// Parses the URL query string into a typed struct using `serde_urlencoded`.
/// Returns 400 Bad Request if parsing fails.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[derive(Deserialize)]
/// struct Pagination {
///     page: Option<u32>,
///     limit: Option<u32>,
/// }
///
/// #[get("/users")]
/// async fn list_users(query: Query<Pagination>) -> String {
///     let page = query.page.unwrap_or(1);
///     format!("Page: {}", page)
/// }
/// ```
#[derive(Debug)]
pub struct Query<T>(pub T);

/// Extracts and deserializes URL-encoded form data.
///
/// Parses `application/x-www-form-urlencoded` request bodies.
/// Returns 400 Bad Request if content-type is wrong or parsing fails.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[derive(Deserialize)]
/// struct LoginForm {
///     username: String,
///     password: String,
/// }
///
/// #[post("/login")]
/// async fn login(form: Form<LoginForm>) -> String {
///     format!("Welcome, {}", form.username)
/// }
/// ```
#[derive(Debug)]
pub struct Form<T>(pub T);

/// Provides access to request headers.
///
/// Extracts all HTTP headers from the request.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[get("/auth")]
/// async fn check_auth(headers: Headers) -> Result<String> {
///     let auth = headers.get("authorization")
///         .ok_or_else(|| Error::unauthorized("missing auth header"))?;
///     Ok("Authenticated".to_string())
/// }
/// ```
#[derive(Debug)]
pub struct Headers(pub http::HeaderMap);

/// Extracts and deserializes cookies from the request.
///
/// Parses the `Cookie` header into a typed struct. Each field in the struct
/// corresponds to a cookie name. Returns 400 Bad Request if parsing fails.
///
/// Use `Option<Cookie<T>>` for optional cookie access.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[derive(Deserialize)]
/// struct Session {
///     session_id: String,
/// }
///
/// #[get("/dashboard")]
/// async fn dashboard(session: Cookie<Session>) -> Result<Json<Dashboard>> {
///     // Use session.session_id...
/// }
/// ```
#[derive(Debug)]
pub struct Cookie<T>(pub T);

/// Extracts application state.
///
/// Provides access to shared application state that was registered
/// with [`Rapina::state`](crate::app::Rapina::state).
///
/// The inner value is wrapped in an `Arc<T>`, so extraction is always
/// a cheap atomic reference-count bump rather than a deep clone.
/// This also removes the `Clone` requirement on `T`.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// struct AppConfig {
///     db_url: String,
/// }
///
/// #[get("/config")]
/// async fn get_config(state: State<AppConfig>) -> String {
///     state.db_url.clone()
/// }
/// ```
#[derive(Debug, Clone)]
pub struct State<T>(pub Arc<T>);

/// Provides access to the request context.
///
/// Contains the `trace_id` and request start time for logging and tracing.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[get("/trace")]
/// async fn get_trace(ctx: Context) -> String {
///     format!("Trace ID: {}", ctx.trace_id())
/// }
/// ```
#[derive(Debug)]
pub struct Context(pub RequestContext);

/// Wraps an extractor and validates the extracted value.
///
/// Uses the `validator` crate to run validation rules on the inner value.
/// Returns 422 Validation Error if validation fails.
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
/// #[derive(Deserialize, Validate)]
/// struct CreateUser {
///     #[validate(email)]
///     email: String,
///     #[validate(length(min = 8))]
///     password: String,
/// }
///
/// #[post("/users")]
/// async fn create_user(body: Validated<Json<CreateUser>>) -> String {
///     // data is guaranteed to be valid
///     format!("Created user: {}", body.email)
/// }
/// ```
#[derive(Debug)]
pub struct Validated<T>(pub T);

/// Path parameters extracted from the URL during route matching.
///
/// Stores up to 4 parameters on the stack without heap allocation.
/// Uses linear search which outperforms HashMap for the small N
/// typical of REST APIs (1-3 path parameters).
#[derive(Debug, Clone, Default)]
pub struct PathParams {
    entries: SmallVec<[(Cow<'static, str>, String); 4]>,
}

impl PathParams {
    pub fn new() -> Self {
        Self {
            entries: SmallVec::new(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&String> {
        self.entries
            .iter()
            .find(|(k, _)| k.as_ref() == key)
            .map(|(_, v)| v)
    }

    pub fn insert(&mut self, key: String, value: String) -> Option<String> {
        if let Some(entry) = self.entries.iter_mut().find(|(k, _)| k.as_ref() == key) {
            let old = std::mem::replace(&mut entry.1, value);
            Some(old)
        } else {
            self.entries.push((Cow::Owned(key), value));
            None
        }
    }

    /// Push a static key without checking for duplicates.
    ///
    /// Used by the trie during route matching where param names are
    /// leaked to `&'static str` at freeze time and the same key is never
    /// inserted twice in a single lookup. Zero allocation for the key,
    /// and skips the linear scan that `insert()` does.
    pub(crate) fn push(&mut self, key: &'static str, value: String) {
        self.entries.push((Cow::Borrowed(key), value));
    }

    pub fn remove(&mut self, key: &str) -> Option<String> {
        if let Some(pos) = self.entries.iter().position(|(k, _)| k.as_ref() == key) {
            Some(self.entries.swap_remove(pos).1)
        } else {
            None
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &String)> {
        self.entries.iter().map(|(k, v)| (k.as_ref(), v))
    }
}

impl FromIterator<(String, String)> for PathParams {
    fn from_iter<I: IntoIterator<Item = (String, String)>>(iter: I) -> Self {
        Self {
            entries: iter.into_iter().map(|(k, v)| (Cow::Owned(k), v)).collect(),
        }
    }
}

/// Trait for extractors that consume the request body.
///
/// Implement this trait for extractors that need access to the full request,
/// including the body. Only one body-consuming extractor can be used per handler,
/// and it **must be the last parameter** in the handler function signature.
pub trait FromRequest: Sized {
    /// Extract the value from the request.
    fn from_request(
        req: Request<Incoming>,
        params: &PathParams,
        state: &Arc<AppState>,
    ) -> impl std::future::Future<Output = Result<Self, Error>> + Send;
}

/// Trait for extractors that only need request metadata.
///
/// Implement this trait for extractors that don't need the request body,
/// such as path parameters, query strings, or headers.
/// Multiple parts-only extractors can be used in a single handler
/// and must appear before any body-consuming extractor.
pub trait FromRequestParts: Sized + Send {
    /// Extract the value from request parts.
    fn from_request_parts(
        parts: &http::request::Parts,
        params: &PathParams,
        state: &Arc<AppState>,
    ) -> impl std::future::Future<Output = Result<Self, Error>> + Send;
}

impl<T> Json<T> {
    /// Consumes the extractor and returns the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Path<T> {
    /// Consumes the extractor and returns the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Query<T> {
    /// Consumes the extractor and returns the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> Form<T> {
    /// Consumes the extractor and returns the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl Headers {
    /// Gets a header value by name.
    pub fn get(&self, key: &str) -> Option<&http::HeaderValue> {
        self.0.get(key)
    }

    /// Consumes the extractor and returns the inner HeaderMap.
    pub fn into_inner(self) -> http::HeaderMap {
        self.0
    }
}

impl<T> Cookie<T> {
    /// Consumes the extractor and returns the inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> State<T> {
    /// Consumes the extractor and returns the inner `Arc<T>`.
    pub fn into_inner(self) -> Arc<T> {
        self.0
    }
}

impl Context {
    /// Consumes the extractor and returns the inner RequestContext.
    pub fn into_inner(self) -> RequestContext {
        self.0
    }

    /// Returns the trace ID for this request.
    pub fn trace_id(&self) -> &str {
        self.0.trace_id()
    }

    /// Returns the elapsed time since the request started.
    pub fn elapsed(&self) -> std::time::Duration {
        self.0.elapsed()
    }
}

impl<T> Validated<T> {
    /// Consumes the extractor and returns the validated inner value.
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T: DeserializeOwned + Send> FromRequest for Json<T> {
    async fn from_request(
        req: Request<Incoming>,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let body = req.into_body();
        let bytes = body
            .collect()
            .await
            .map_err(|_| Error::bad_request("Failed to read request body"))?
            .to_bytes();

        let value: T = serde_json::from_slice(&bytes)
            .map_err(|e| Error::bad_request(format!("Invalid JSON in request body: {}", e)))?;

        Ok(Json(value))
    }
}

impl<T: serde::Serialize> IntoResponse for (http::StatusCode, Json<T>) {
    fn into_response(self) -> http::Response<BoxBody> {
        let body = serde_json::to_vec(&(self.1).0).unwrap_or_default();
        http::Response::builder()
            .status(self.0)
            .header(CONTENT_TYPE, APPLICATION_JSON)
            .body(crate::response::full(body))
            .unwrap()
    }
}

impl<T: serde::Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> http::Response<BoxBody> {
        (http::StatusCode::OK, self).into_response()
    }
}

impl<T: DeserializeOwned + Send> FromRequest for Form<T> {
    async fn from_request(
        req: Request<Incoming>,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let content_type = req
            .headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok());

        if !content_type
            .map(|ct| ct.starts_with(FORM_CONTENT_TYPE))
            .unwrap_or(false)
        {
            return Err(Error::bad_request(format!(
                "Expected Content-Type '{}', got '{}'",
                FORM_CONTENT_TYPE,
                content_type.unwrap_or("none")
            )));
        }

        let body = req.into_body();
        let bytes = body
            .collect()
            .await
            .map_err(|_| Error::bad_request("Failed to read form data from request body"))?
            .to_bytes();

        let value: T = serde_urlencoded::from_bytes(&bytes)
            .map_err(|e| Error::bad_request(format!("Invalid URL-encoded form data: {}", e)))?;

        Ok(Form(value))
    }
}

impl<T: DeserializeOwned + Validate + Send> FromRequest for Validated<Json<T>> {
    async fn from_request(
        req: Request<Incoming>,
        params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let json = Json::<T>::from_request(req, params, state).await?;
        json.0.validate().map_err(|e| {
            Error::validation("validation failed")
                .with_details(serde_json::to_value(e).unwrap_or_default())
        })?;
        Ok(Validated(json))
    }
}

impl<T: DeserializeOwned + Validate + Send> FromRequest for Validated<Form<T>> {
    async fn from_request(
        req: Request<Incoming>,
        params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let form = Form::<T>::from_request(req, params, state).await?;
        form.0.validate().map_err(|e| {
            Error::validation("validation failed")
                .with_details(serde_json::to_value(e).unwrap_or_default())
        })?;
        Ok(Validated(form))
    }
}

impl<T: Send + Sync + 'static> FromRequestParts for State<T> {
    async fn from_request_parts(
        _parts: &http::request::Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let arc = state.get_arc::<T>().ok_or_else(|| {
            Error::internal(format!(
                "State not registered for type '{}'. Did you forget to call .state() or .state_arc()?",
                std::any::type_name::<T>()
            ))
        })?;
        Ok(State(arc))
    }
}

impl FromRequestParts for Context {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        parts
            .extensions
            .get::<RequestContext>()
            .cloned()
            .map(Context)
            .ok_or_else(|| {
                Error::internal(
                    "RequestContext missing from request extensions. \
                     The request pipeline did not initialize the request context.",
                )
            })
    }
}

impl<T: DeserializeOwned + Send> FromRequestParts for Query<T> {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let query = parts.uri.query().unwrap_or("");
        let value: T = serde_urlencoded::from_str(query)
            .map_err(|e| Error::bad_request(format!("Invalid query string parameters: {}", e)))?;
        Ok(Query(value))
    }
}

impl FromRequestParts for Headers {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        Ok(Headers(parts.headers.clone()))
    }
}

impl<T: DeserializeOwned + Send> FromRequestParts for Cookie<T> {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let cookie_header = parts
            .headers
            .get(http::header::COOKIE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");

        // Parse cookies into key=value pairs
        let cookies: HashMap<String, String> = cookie_header
            .split(';')
            .filter_map(|pair| {
                let mut parts = pair.trim().splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next()?.to_string();
                if key.is_empty() {
                    None
                } else {
                    Some((key, value))
                }
            })
            .collect();

        // Serialize to JSON then deserialize to target type
        let json = serde_json::to_string(&cookies)
            .map_err(|e| Error::bad_request(format!("Failed to process cookies: {}", e)))?;

        let value: T = serde_json::from_str(&json)
            .map_err(|e| Error::bad_request(format!("Invalid or missing cookies: {}", e)))?;

        Ok(Cookie(value))
    }
}

// ── PathParamsDeserializer ────────────────────────────────────────────────────
//
// Teaches serde how to read from PathParams so a single
// `impl<T: DeserializeOwned> FromRequestParts for Path<T>` can handle:
//   - Path<u64> / Path<String>  → first param value
//   - Path<(u64, String)>       → params in insertion order (SeqAccess)
//   - Path<MyStruct>            → params by field name (MapAccess)

#[derive(Debug)]
struct PathDeError(String);

impl fmt::Display for PathDeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PathDeError {}

impl de::Error for PathDeError {
    fn custom<T: fmt::Display>(msg: T) -> Self {
        PathDeError(msg.to_string())
    }
}

struct PathParamsDeserializer<'de>(&'de PathParams);

impl<'de> PathParamsDeserializer<'de> {
    fn first_str(&self) -> Result<&'de str, PathDeError> {
        self.0
            .iter()
            .next()
            .map(|(_, v)| v.as_str())
            .ok_or_else(|| {
                de::Error::custom(
                    "Missing path parameter. Ensure your route pattern includes a parameter like /:id",
                )
            })
    }
}

// Generates scalar deserialize methods for PathParamsDeserializer by delegating
// to StrDeserializer, which holds the actual parse logic.
macro_rules! delegate_scalar {
    ($($method:ident),+ $(,)?) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
            StrDeserializer(self.first_str()?).$method(visitor)
        })+
    };
}

impl<'de> de::Deserializer<'de> for PathParamsDeserializer<'de> {
    type Error = PathDeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        match self.0.len() {
            0 => Err(de::Error::custom("missing path parameter")),
            1 => visitor.visit_str(self.first_str()?),
            _ => self.deserialize_seq(visitor),
        }
    }

    delegate_scalar! {
        deserialize_bool,
        deserialize_i8, deserialize_i16, deserialize_i32, deserialize_i64, deserialize_i128,
        deserialize_u8, deserialize_u16, deserialize_u32, deserialize_u64, deserialize_u128,
        deserialize_f32, deserialize_f64,
        deserialize_str, deserialize_string, deserialize_identifier,
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        StrDeserializer(self.first_str()?).deserialize_option(visitor)
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathDeError> {
        StrDeserializer(self.first_str()?).deserialize_newtype_struct(name, visitor)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_seq(PathParamsSeqAccess {
            iter: self.0.entries.iter(),
        })
    }
    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, PathDeError> {
        self.deserialize_seq(visitor)
    }
    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, PathDeError> {
        self.deserialize_seq(visitor)
    }
    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_map(PathParamsMapAccess {
            iter: self.0.entries.iter(),
            pending_value: None,
        })
    }
    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, PathDeError> {
        self.deserialize_map(visitor)
    }

    serde::forward_to_deserialize_any! {
        char bytes byte_buf unit unit_struct enum ignored_any
    }
}

// Iterates param values in insertion order (for tuples).
struct PathParamsSeqAccess<'de> {
    iter: std::slice::Iter<'de, (Cow<'static, str>, String)>,
}

impl<'de> SeqAccess<'de> for PathParamsSeqAccess<'de> {
    type Error = PathDeError;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, PathDeError> {
        match self.iter.next() {
            None => Ok(None),
            Some((_, val)) => seed.deserialize(StrDeserializer(val.as_str())).map(Some),
        }
    }
}

// Iterates param key-value pairs (for structs).
struct PathParamsMapAccess<'de> {
    iter: std::slice::Iter<'de, (Cow<'static, str>, String)>,
    pending_value: Option<&'de str>,
}

impl<'de> MapAccess<'de> for PathParamsMapAccess<'de> {
    type Error = PathDeError;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, PathDeError> {
        match self.iter.next() {
            None => Ok(None),
            Some((key, val)) => {
                self.pending_value = Some(val.as_str());
                seed.deserialize(StrDeserializer(key.as_ref())).map(Some)
            }
        }
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(
        &mut self,
        seed: V,
    ) -> Result<V::Value, PathDeError> {
        let val = self
            .pending_value
            .take()
            .ok_or_else(|| de::Error::custom("next_value called before next_key"))?;
        seed.deserialize(StrDeserializer(val))
    }
}

// Deserializes a single &str into any primitive — used by SeqAccess and MapAccess.
struct StrDeserializer<'de>(&'de str);

// Generates parse-and-visit methods for StrDeserializer.
macro_rules! parse_scalar {
    ($($method:ident => $visit:ident: $ty:ty),+ $(,)?) => {
        $(fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
            visitor.$visit(self.0.parse::<$ty>().map_err(de::Error::custom)?)
        })+
    };
}

impl<'de> de::Deserializer<'de> for StrDeserializer<'de> {
    type Error = PathDeError;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_str(self.0)
    }

    parse_scalar! {
        deserialize_bool  => visit_bool:  bool,
        deserialize_i8    => visit_i8:    i8,
        deserialize_i16   => visit_i16:   i16,
        deserialize_i32   => visit_i32:   i32,
        deserialize_i64   => visit_i64:   i64,
        deserialize_i128  => visit_i128:  i128,
        deserialize_u8    => visit_u8:    u8,
        deserialize_u16   => visit_u16:   u16,
        deserialize_u32   => visit_u32:   u32,
        deserialize_u64   => visit_u64:   u64,
        deserialize_u128  => visit_u128:  u128,
        deserialize_f32   => visit_f32:   f32,
        deserialize_f64   => visit_f64:   f64,
    }

    fn deserialize_str<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_str(self.0)
    }
    fn deserialize_string<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_string(self.0.to_owned())
    }
    fn deserialize_identifier<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_str(self.0)
    }
    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, PathDeError> {
        visitor.visit_some(self)
    }
    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, PathDeError> {
        visitor.visit_newtype_struct(self)
    }

    serde::forward_to_deserialize_any! {
        char bytes byte_buf unit unit_struct seq tuple tuple_struct
        map struct enum ignored_any
    }
}

impl<T: DeserializeOwned + Send> FromRequestParts for Path<T> {
    async fn from_request_parts(
        _parts: &http::request::Parts,
        params: &PathParams,
        _state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        T::deserialize(PathParamsDeserializer(params))
            .map(Path)
            .map_err(|e| Error::bad_request(e.to_string()))
    }
}

impl<T: FromRequestParts> FromRequest for T {
    async fn from_request(
        req: Request<Incoming>,
        params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let (parts, _body) = req.into_parts();
        Self::from_request_parts(&parts, params, state).await
    }
}

pub fn extract_path_params(pattern: &str, path: &str) -> Option<PathParams> {
    let pattern_parts: Vec<&str> = pattern.split('/').collect();
    let path_parts: Vec<&str> = path.split('/').collect();

    if pattern_parts.len() != path_parts.len() {
        return None;
    }

    let mut params = PathParams::new();

    for (pattern_part, path_part) in pattern_parts.iter().zip(path_parts.iter()) {
        if let Some(param_name) = pattern_part.strip_prefix(':') {
            params.insert(param_name.to_string(), path_part.to_string());
        } else if pattern_part != path_part {
            return None;
        }
    }

    Some(params)
}

macro_rules! impl_deref {
    ($name:ident) => {
        impl<T> Deref for $name<T> {
            type Target = T;
            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }
    };
}

impl<T> Deref for State<T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl_deref!(Json);
impl_deref!(Path);
impl_deref!(Query);
impl_deref!(Form);
impl_deref!(Cookie);
impl_deref!(Validated);

impl Deref for Context {
    type Target = RequestContext;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for Headers {
    type Target = http::HeaderMap;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

// Database extractor (requires "database" feature)
#[cfg(feature = "database")]
impl FromRequestParts for crate::database::Db {
    async fn from_request_parts(
        _parts: &http::request::Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        use sea_orm::DatabaseConnection;

        let conn = state.get::<DatabaseConnection>().ok_or_else(|| {
            Error::internal(
                "Database connection not configured. Did you forget to call .with_database()?",
            )
        })?;
        Ok(crate::database::Db::new(conn.clone()))
    }
}

// Jobs extractor (requires "database" feature)
#[cfg(feature = "database")]
impl FromRequestParts for crate::jobs::Jobs {
    async fn from_request_parts(
        parts: &http::request::Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        use sea_orm::DatabaseConnection;

        let pool = state
            .get::<DatabaseConnection>()
            .ok_or_else(|| {
                Error::internal(
                    "Database connection not configured. Did you forget to call .with_database()?",
                )
            })?
            .clone();

        let trace_id = parts
            .extensions
            .get::<RequestContext>()
            .map(|ctx| ctx.trace_id().to_owned());

        Ok(crate::jobs::Jobs::new(pool, trace_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test::{TestRequest, empty_params, empty_state, params};

    #[derive(Debug, Clone, PartialEq)]
    struct Data {
        name: String,
    }

    // Path params extraction tests
    #[test]
    fn test_extract_path_params_exact_match() {
        let result = extract_path_params("/users", "/users");
        assert!(result.is_some());
        assert!(result.unwrap().is_empty());
    }

    #[test]
    fn test_extract_path_params_single_param() {
        let result = extract_path_params("/users/:id", "/users/123");
        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.get("id"), Some(&"123".to_string()));
    }

    #[test]
    fn test_extract_path_params_multiple_params() {
        let result = extract_path_params("/users/:user_id/posts/:post_id", "/users/1/posts/42");
        assert!(result.is_some());
        let params = result.unwrap();
        assert_eq!(params.get("user_id"), Some(&"1".to_string()));
        assert_eq!(params.get("post_id"), Some(&"42".to_string()));
    }

    #[test]
    fn test_extract_path_params_no_match_different_length() {
        let result = extract_path_params("/users/:id", "/users/123/extra");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_path_params_no_match_different_static() {
        let result = extract_path_params("/users/:id", "/posts/123");
        assert!(result.is_none());
    }

    #[test]
    fn test_extract_path_params_root() {
        let result = extract_path_params("/", "/");
        assert!(result.is_some());
    }

    // Query extractor tests
    #[tokio::test]
    async fn test_query_extractor_success() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Params {
            page: u32,
            limit: u32,
        }

        let (parts, _) = TestRequest::get("/users?page=1&limit=10").into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let query = result.unwrap();
        assert_eq!(query.0.page, 1);
        assert_eq!(query.0.limit, 10);
    }

    #[tokio::test]
    async fn test_query_extractor_optional_fields() {
        #[derive(serde::Deserialize)]
        struct Params {
            page: Option<u32>,
            search: Option<String>,
        }

        let (parts, _) = TestRequest::get("/users?page=5").into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let query = result.unwrap();
        assert_eq!(query.0.page, Some(5));
        assert!(query.0.search.is_none());
    }

    #[tokio::test]
    async fn test_query_extractor_empty_query() {
        #[allow(dead_code)]
        #[derive(serde::Deserialize, Default)]
        struct Params {
            #[serde(default)]
            page: u32,
        }

        let (parts, _) = TestRequest::get("/users").into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_query_extractor_invalid_type() {
        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Params {
            page: u32,
        }

        let (parts, _) = TestRequest::get("/users?page=notanumber").into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status(), 400);
    }

    #[tokio::test]
    async fn test_query_extractor_uuid() {
        #[derive(serde::Deserialize, PartialEq, Debug)]
        struct Params {
            id: uuid::Uuid,
        }

        let id = uuid::Uuid::new_v4();
        let (parts, _) = TestRequest::get(&format!("/users?id={id}")).into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap().0.id, id);
    }

    #[tokio::test]
    async fn test_query_extractor_uuid_invalid() {
        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Params {
            id: uuid::Uuid,
        }

        let (parts, _) = TestRequest::get("/users?id=not-a-uuid").into_parts();
        let result =
            Query::<Params>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 400);
    }

    // Headers extractor tests
    #[tokio::test]
    async fn test_headers_extractor() {
        let (parts, _) = TestRequest::get("/")
            .header("x-custom", "value")
            .header("authorization", "Bearer token")
            .into_parts();

        let result = Headers::from_request_parts(&parts, &empty_params(), &empty_state()).await;
        assert!(result.is_ok());

        let headers = result.unwrap();
        assert_eq!(headers.get("x-custom").unwrap().to_str().unwrap(), "value");
        assert_eq!(
            headers.get("authorization").unwrap().to_str().unwrap(),
            "Bearer token"
        );
    }

    #[tokio::test]
    async fn test_headers_extractor_missing_header() {
        let (parts, _) = TestRequest::get("/").into_parts();
        let result = Headers::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let headers = result.unwrap();
        assert!(headers.get("x-nonexistent").is_none());
    }

    // Path extractor tests
    #[tokio::test]
    async fn test_path_extractor_u64() {
        let (parts, _) = TestRequest::get("/users/123").into_parts();
        let params = params(&[("id", "123")]);

        let result = Path::<u64>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().0, 123);
    }

    #[tokio::test]
    async fn test_path_extractor_string() {
        let (parts, _) = TestRequest::get("/users/john").into_parts();
        let params = params(&[("name", "john")]);

        let result = Path::<String>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().0, "john");
    }

    #[tokio::test]
    async fn test_path_extractor_invalid_type() {
        let (parts, _) = TestRequest::get("/users/notanumber").into_parts();
        let params = params(&[("id", "notanumber")]);

        let result = Path::<u64>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 400);
    }

    #[tokio::test]
    async fn test_path_extractor_missing_param() {
        let (parts, _) = TestRequest::get("/users").into_parts();
        let params = empty_params();

        let result = Path::<u64>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_path_extractor_uuid() {
        let id = uuid::Uuid::new_v4();
        let (parts, _) = TestRequest::get(&format!("/users/{id}")).into_parts();
        let params = params(&[("id", &id.to_string())]);

        let result = Path::<uuid::Uuid>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().0, id);
    }

    #[tokio::test]
    async fn test_path_extractor_uuid_invalid() {
        let (parts, _) = TestRequest::get("/users/not-a-uuid").into_parts();
        let params = params(&[("id", "not-a-uuid")]);

        let result = Path::<uuid::Uuid>::from_request_parts(&parts, &params, &empty_state()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 400);
    }

    // Context extractor tests
    #[tokio::test]
    async fn test_context_extractor() {
        let (parts, _) = TestRequest::get("/").into_parts();
        let result = Context::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let ctx = result.unwrap();
        assert!(!ctx.trace_id().is_empty());
    }

    #[tokio::test]
    async fn test_context_extractor_with_custom_trace_id() {
        let custom_ctx = crate::context::RequestContext::with_trace_id("custom-123".to_string());
        let (parts, _) = TestRequest::get("/").into_parts_with_context(custom_ctx);

        let result = Context::from_request_parts(&parts, &empty_params(), &empty_state()).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().trace_id(), "custom-123");
    }

    // State extractor tests
    #[tokio::test]
    async fn test_state_extractor_success() {
        struct AppConfig {
            name: String,
        }

        let state = crate::test::state_with(AppConfig {
            name: "test-app".to_string(),
        });
        let (parts, _) = TestRequest::get("/").into_parts();

        let result = State::<AppConfig>::from_request_parts(&parts, &empty_params(), &state).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name, "test-app");
    }

    #[tokio::test]
    async fn test_state_extractor_not_found() {
        #[derive(Debug)]
        struct MissingState;

        let state = empty_state();
        let (parts, _) = TestRequest::get("/").into_parts();

        let result =
            State::<MissingState>::from_request_parts(&parts, &empty_params(), &state).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 500);
    }

    #[tokio::test]
    async fn test_state_extractor_arc_trait_object() {
        trait Greeter: Send + Sync {
            fn greet(&self) -> &'static str;
        }

        struct Hello;
        impl Greeter for Hello {
            fn greet(&self) -> &'static str {
                "hello"
            }
        }

        let greeter: std::sync::Arc<dyn Greeter> = std::sync::Arc::new(Hello);
        let state = std::sync::Arc::new(crate::state::AppState::new().with_arc(greeter));
        let (parts, _) = TestRequest::get("/").into_parts();

        let result = State::<std::sync::Arc<dyn Greeter>>::from_request_parts(
            &parts,
            &empty_params(),
            &state,
        )
        .await;

        assert!(result.is_ok());
        // Deref chain: State<Arc<dyn Greeter>> -> Arc<dyn Greeter> -> dyn Greeter
        assert_eq!(result.unwrap().greet(), "hello");
    }

    // into_inner tests
    #[test]
    fn test_json_into_inner() {
        let json = Json("value".to_string());
        assert_eq!(json.into_inner(), "value");
    }

    #[test]
    fn test_path_into_inner() {
        let path = Path(42u64);
        assert_eq!(path.into_inner(), 42);
    }

    #[test]
    fn test_query_into_inner() {
        let query = Query("test".to_string());
        assert_eq!(query.into_inner(), "test");
    }

    #[test]
    fn test_form_into_inner() {
        let form = Form("data".to_string());
        assert_eq!(form.into_inner(), "data");
    }

    #[test]
    fn test_headers_into_inner() {
        let headers = Headers(http::HeaderMap::new());
        let inner = headers.into_inner();
        assert!(inner.is_empty());
    }

    #[test]
    fn test_state_into_inner() {
        let state = State(Arc::new("value".to_string()));
        let arc = state.into_inner();
        assert_eq!(*arc, "value");
    }

    #[test]
    fn test_context_into_inner() {
        let ctx = crate::context::RequestContext::with_trace_id("test".to_string());
        let context = Context(ctx);
        assert_eq!(context.into_inner().trace_id(), "test");
    }

    #[test]
    fn test_context_elapsed() {
        let ctx = crate::context::RequestContext::new();
        let context = Context(ctx);
        // Verify elapsed() returns a Duration (compile-time check)
        let _elapsed: std::time::Duration = context.elapsed();
    }

    #[test]
    fn test_validated_into_inner() {
        let validated = Validated("value".to_string());
        assert_eq!(validated.into_inner(), "value");
    }

    #[test]
    fn test_validated_with_struct() {
        #[derive(Debug, PartialEq)]
        struct Data {
            name: String,
        }

        let validated = Validated(Data {
            name: "test".to_string(),
        });
        assert_eq!(
            validated.into_inner(),
            Data {
                name: "test".to_string()
            }
        );
    }

    // deref tests
    #[test]
    fn test_json_deref() {
        let json = Json("value".to_string());
        assert_eq!(*json, "value");
    }

    #[test]
    fn test_path_deref() {
        let path = Path(42u64);
        assert_eq!(*path, 42);
    }

    #[test]
    fn test_query_deref() {
        let query = Query("test".to_string());
        assert_eq!(*query, "test");
    }

    #[test]
    fn test_form_deref() {
        let form = Form("data".to_string());
        assert_eq!(*form, "data");
    }

    #[test]
    fn test_state_deref() {
        let state = State(Arc::new("value".to_string()));
        assert_eq!(*state, "value");
    }

    #[test]
    fn test_validated_deref() {
        let validated = Validated("value".to_string());
        assert_eq!(*validated, "value");
    }

    #[test]
    fn test_validated_deref_with_struct() {
        let validated = Validated(Data {
            name: "test".to_string(),
        });
        assert_eq!(
            *validated,
            Data {
                name: "test".to_string()
            }
        );
    }

    // autoderef tests
    #[test]
    fn test_json_autoderef() {
        let data = Data {
            name: "json test".to_string(),
        };

        let json = Json(data.clone());
        assert_eq!(json.name, data.name);
    }

    #[test]
    fn test_state_autoderef() {
        let data = Data {
            name: "state test".to_string(),
        };

        let state = State(Arc::new(data.clone()));
        assert_eq!(state.name, data.name);
    }

    #[test]
    fn test_form_autoderef() {
        let data = Data {
            name: "form test".to_string(),
        };

        let form = Form(data.clone());
        assert_eq!(form.name, data.name);
    }

    #[test]
    fn test_headers_autoderef() {
        let headers = Headers(http::HeaderMap::new());
        assert!(headers.is_empty());
    }

    #[test]
    fn test_context_autoderef() {
        let ctx = Context(crate::context::RequestContext::with_trace_id(
            "test".to_string(),
        ));
        assert_eq!(ctx.trace_id(), "test");
    }

    #[test]
    fn test_validated_autoderef() {
        let data = Data {
            name: "test".to_string(),
        };

        let validated = Validated(data.clone());
        assert_eq!(validated.name, data.name);
    }

    // Cookie extractor tests
    #[tokio::test]
    async fn test_cookie_extractor_success() {
        #[derive(serde::Deserialize, Debug, PartialEq)]
        struct Session {
            session_id: String,
        }

        let (parts, _) = TestRequest::get("/dashboard")
            .header("cookie", "session_id=abc123")
            .into_parts();

        let result =
            Cookie::<Session>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let cookie = result.unwrap();
        assert_eq!(cookie.0.session_id, "abc123");
    }

    #[tokio::test]
    async fn test_cookie_extractor_multiple_cookies() {
        #[derive(serde::Deserialize, Debug)]
        struct Cookies {
            session_id: String,
            user_id: String,
        }

        let (parts, _) = TestRequest::get("/")
            .header("cookie", "session_id=abc123; user_id=user456")
            .into_parts();

        let result =
            Cookie::<Cookies>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let cookies = result.unwrap();
        assert_eq!(cookies.0.session_id, "abc123");
        assert_eq!(cookies.0.user_id, "user456");
    }

    #[tokio::test]
    async fn test_cookie_extractor_optional_field() {
        #[derive(serde::Deserialize, Debug)]
        struct Cookies {
            session_id: String,
            tracking: Option<String>,
        }

        let (parts, _) = TestRequest::get("/")
            .header("cookie", "session_id=abc123")
            .into_parts();

        let result =
            Cookie::<Cookies>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        let cookies = result.unwrap();
        assert_eq!(cookies.0.session_id, "abc123");
        assert!(cookies.0.tracking.is_none());
    }

    #[tokio::test]
    async fn test_cookie_extractor_missing_required() {
        // Struct never successfully deserializes in this test (testing error case)
        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Session {
            session_id: String,
        }

        let (parts, _) = TestRequest::get("/").into_parts();

        let result =
            Cookie::<Session>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_err());
        let err = result.unwrap_err();
        assert_eq!(err.status(), 400);
        assert!(err.message().contains("session_id"));
    }

    #[tokio::test]
    async fn test_cookie_extractor_empty_header() {
        #[allow(dead_code)]
        #[derive(serde::Deserialize, Debug)]
        struct Session {
            session_id: Option<String>,
        }

        let (parts, _) = TestRequest::get("/").header("cookie", "").into_parts();

        let result =
            Cookie::<Session>::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        assert!(result.is_ok());
        assert!(result.unwrap().0.session_id.is_none());
    }

    #[test]
    fn test_cookie_into_inner() {
        let cookie = Cookie("session".to_string());
        assert_eq!(cookie.into_inner(), "session");
    }

    #[tokio::test]
    async fn test_path_tuple_two_params() {
        let p = params(&[("org_id", "10"), ("team_id", "42")]);
        let result = Path::<(u64, u64)>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().0, (10, 42));
    }

    #[tokio::test]
    async fn test_path_tuple_three_params() {
        let p = params(&[("org_id", "1"), ("team_id", "2"), ("member_id", "3")]);
        let result = Path::<(u64, u64, u64)>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().0, (1, 2, 3));
    }

    #[tokio::test]
    async fn test_path_tuple_missing_param() {
        let p = params(&[("org_id", "1")]);
        let result = Path::<(u64, u64)>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_path_tuple_bad_parse() {
        let p = params(&[("org_id", "not_a_number"), ("team_id", "2")]);
        let result = Path::<(u64, u64)>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_path_struct_params() {
        #[derive(serde::Deserialize)]
        struct OrgParams {
            org_id: u64,
            team_id: u64,
        }

        let p = params(&[("org_id", "10"), ("team_id", "42")]);
        let result = Path::<OrgParams>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_ok());
        let Path(v) = result.unwrap();
        assert_eq!(v.org_id, 10);
        assert_eq!(v.team_id, 42);
    }

    #[tokio::test]
    async fn test_path_struct_bad_parse() {
        #[derive(Debug, serde::Deserialize)]
        #[allow(dead_code)]
        struct OrgParams {
            org_id: u64,
            team_id: u64,
        }

        let p = params(&[("org_id", "not_a_number"), ("team_id", "42")]);
        let result = Path::<OrgParams>::from_request_parts(
            &TestRequest::get("/").into_parts().0,
            &p,
            &empty_state(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 400);
    }

    #[cfg(feature = "database")]
    #[tokio::test]
    async fn jobs_extractor_missing_db_returns_500() {
        let (parts, _) = TestRequest::get("/").into_parts();
        let result =
            crate::jobs::Jobs::from_request_parts(&parts, &empty_params(), &empty_state()).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().status(), 500);
    }
}
