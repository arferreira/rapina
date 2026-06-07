use crate::error::Error;
use crate::extract::{FromRequestParts, PathParams};
use crate::jwt;
use crate::jwt::JwksClient;
use crate::state::AppState;
use http::request::Parts;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{DecodingKey, Header, Validation, decode, decode_header};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;

#[derive(Debug, Deserialize)]
pub struct DefaultClaims {}

/// Extracts a JSON Web Token (JWT) from the `Authorization` request header. The header value can optionally be prefixed with `Bearer `.
///
/// Parses the `Authorization` header and validates the structure of the header value to be a JSON Web Token
/// Returns 400 Bad Request if the header value is empty or parsing fails.
/// Returns 401 Unauthorized if the `kid` of the JWT is missing in the configured JWKS or the JWT fails the validation based on the configured `validation` (covering token expiration, token audience, ...)
///
/// # Examples
///
/// ```ignore
/// use rapina::prelude::*;
///
///
/// #[get("/config")]
/// async fn get_config(token: JsonWebToken) -> StatusCode {
///     StatusCode::Ok
/// }
/// ```
#[derive(Debug, Deserialize)]
pub struct JsonWebToken<T = DefaultClaims> {
    /// Subject
    pub sub: String,
    /// Issuer
    #[serde(default)]
    pub iss: Option<String>,
    /// Audience
    #[serde(default)]
    pub aud: Option<String>,
    /// Expiration time
    pub exp: usize,
    /// Issued at
    #[serde(default)]
    pub iat: Option<usize>,
    /// Not before
    #[serde(default)]
    pub nbf: Option<usize>,
    #[serde(flatten)]
    pub claims: T,
}

impl<T> JsonWebToken<T>
where
    T: DeserializeOwned,
{
    pub fn new(
        jwks: JwkSet,
        validation: Option<&Validation>,
        token: String,
    ) -> Result<Self, Error> {
        let token = token.trim_start_matches("Bearer ");
        let jwt_header = Self::parse_header(token)?;

        let Some(kid) = jwt_header.kid else {
            return Err(Error::unauthorized(
                "Token doesn't have a `kid` header field",
            ));
        };

        let Some(jwk) = jwks.find(&kid) else {
            return Err(Error::unauthorized(
                "no matching JWK found for the given `kid`",
            ));
        };

        let validation = if let Some(validation) = validation {
            if validation.validate_aud && validation.aud.is_none() {
                tracing::error!(
                    "JWT misconfiguration: validate_aud is enabled but set_audience() was never called. All requests will fail until this is fixed."
                );
                panic!(
                    "JWT misconfiguration: validate_aud is enabled but set_audience() was never called"
                );
            }

            let mut v = validation.clone();
            v.algorithms = vec![jwt_header.alg];
            v
        } else {
            let mut v = jwt::default_validation();
            v.algorithms = vec![jwt_header.alg];
            v
        };

        let decoding_key = DecodingKey::from_jwk(jwk).map_err(|e| {
            tracing::debug!("Failed to decode JWKS: {}", e);
            Error::unauthorized("Failed to decode JWKS")
        });

        match decode::<JsonWebToken<T>>(token, &decoding_key?, &validation) {
            Ok(decoded_token) => Ok(decoded_token.claims),
            Err(e) => Err(Error::unauthorized(format!(
                "failed to decode token: {}",
                e
            ))),
        }
    }

    fn parse_header(token: &str) -> Result<Header, Error> {
        decode_header(token).map_err(|e| match e.kind() {
            jsonwebtoken::errors::ErrorKind::ExpiredSignature => {
                Error::unauthorized("token expired")
            }
            jsonwebtoken::errors::ErrorKind::InvalidToken => Error::unauthorized("invalid token"),
            _ => Error::unauthorized(format!("token header validation failed: {}", e)),
        })
    }
}

impl<T> FromRequestParts for JsonWebToken<T>
where
    T: DeserializeOwned + Send,
{
    async fn from_request_parts(
        parts: &Parts,
        _params: &PathParams,
        state: &Arc<AppState>,
    ) -> Result<Self, Error> {
        let value: &str = parts
            .headers
            .get(http::header::AUTHORIZATION)
            .ok_or_else(|| Error::unauthorized("missing authorization header"))?
            .to_str()
            .map_err(|_| {
                Error::unauthorized("authorization header could not be parsed as String")
            })?;

        let Some(jwks_client) = state.get::<JwksClient>() else {
            tracing::error!(
                "The Rapina state for JwksClient is empty. Did you forget to call .state(jwks_client)?"
            );
            return Err(Error::internal("internal authentication error"));
        };

        let validation: Option<&Validation> = state.get::<Validation>();

        // Try cache first, fall back to live fetch in case the cache warmup failed on startup
        let jwks: JwkSet = match jwks_client.jwks_content().await {
            Some(jwks) => jwks,
            None => {
                tracing::debug!("JWKS cache is empty, fetching live");
                jwks_client.refresh_jwks_cache().await?;
                match jwks_client.jwks_content().await {
                    Some(jwks) => jwks,
                    None => {
                        tracing::error!("The configured JWKS server is unhealthy");
                        return Err(Error::internal("internal authentication error"));
                    }
                }
            }
        };

        JsonWebToken::new(jwks, validation, value.to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::app::Rapina;
    use crate::error::Error;
    use crate::extract::{FromRequestParts, Json};
    use crate::jwt;
    use crate::jwt::JwksClient;
    use crate::jwt::extract::{DefaultClaims, JsonWebToken};
    use crate::jwt::jwks_client::{direct_for_test, oidc_for_test};
    use crate::prelude::Router;
    use crate::state::AppState;
    use crate::test::{TestRequest, empty_params, empty_state};
    use crate::testing::TestClient;
    use http::header;
    use http::header::AUTHORIZATION;
    use jsonwebtoken::jwk::JwkSet;
    use std::net::SocketAddr;
    use std::sync::Arc;

    const TEST_TOKEN: &str = "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6IjFaNTdkX2k3VEU2S1RZNTdwS3pEeSJ9.eyJpc3MiOiJodHRwczovL2Rldi1kdXp5YXlrNC5ldS5hdXRoMC5jb20vIiwic3ViIjoiNDNxbW44c281R3VFU0U1N0Fkb3BhN09jYTZXeVNidmRAY2xpZW50cyIsImF1ZCI6Imh0dHBzOi8vZGV2LWR1enlheWs0LmV1LmF1dGgwLmNvbS9hcGkvdjIvIiwiaWF0IjoxNjIzNTg1MzAxLCJleHAiOjE2MjM2NzE3MDEsImF6cCI6IjQzcW1uOHNvNUd1RVNFNTdBZG9wYTdPY2E2V3lTYnZkIiwic2NvcGUiOiJyZWFkOnVzZXJzIiwiZ3R5IjoiY2xpZW50LWNyZWRlbnRpYWxzIn0.0MpewU1GgvRqn4F8fK_-Eu70cUgWA5JJrdbJhkCPCxXP-8WwfI-qx1ZQg2a7nbjXICYAEl-Z6z4opgy-H5fn35wGP0wywDqZpqL35IPqx6d0wRvpPMjJM75zVXuIjk7cEhDr2kaf1LOY9auWUwGzPiDB_wM-R0uvUMeRPMfrHaVN73xhAuQWVjCRBHvNscYS5-i6qBQKDMsql87dwR72DgHzMlaC8NnaGREBC-xiSamesqhKPVyGzSkFSaF3ZKpGrSDapqmHkNW9RDBE3GQ9OHM33vzUdVKOjU1g9Leb9PDt0o1U4p3NQoGJPShQ6zgWSUEaqvUZTfkbpD_DoYDRxA";
    const TEST_AUDIENCE: &str = "https://dev-duzyayk4.eu.auth0.com/api/v2/";
    const AUTH0_SAMPLE_JWKS: &str = r#"{"keys":[{"alg":"RS256","kty":"RSA","use":"sig","n":"2V31IZF-EY2GxXQPI5OaEE--sezizPamNZDW9AjBE2cCErfufM312nT2jUsCnfjsXnh6Z_b-ncOMr97zIZkq1ofU7avemv8nX7NpKmoPBpVrMPprOax2-e3wt-bSfFLIHyghjFLKpkT0LOL_Fimi7xY-J86R06WHojLo3yGzAgQCswZmD4CFf6NcBWDcb6l6kx5vk_AdzHIkVEZH4aikUL_fn3zq5qbE25oOg6pT7F7Pp4zdHOAEKnIRS8tvP8tvvVRkUCrjBxz_Kx6Ne1YOD-fkIMRk_MgIWeKZZzZOYx4VrC0vqYiM-PcKWbNdt1kNoTHOeL06XZeSE6WPZ3VB1Q","e":"AQAB","kid":"1Z57d_i7TE6KTY57pKzDy","x5t":"1gA-aTE9VglLXZnrqvzwWhHsFdk","x5c":["MIIDDTCCAfWgAwIBAgIJHwhLfcIbNvmkMA0GCSqGSIb3DQEBCwUAMCQxIjAgBgNVBAMTGWRldi1kdXp5YXlrNC5ldS5hdXRoMC5jb20wHhcNMjEwNjEzMDcxMTQ1WhcNMzUwMjIwMDcxMTQ1WjAkMSIwIAYDVQQDExlkZXYtZHV6eWF5azQuZXUuYXV0aDAuY29tMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA2V31IZF+EY2GxXQPI5OaEE++sezizPamNZDW9AjBE2cCErfufM312nT2jUsCnfjsXnh6Z/b+ncOMr97zIZkq1ofU7avemv8nX7NpKmoPBpVrMPprOax2+e3wt+bSfFLIHyghjFLKpkT0LOL/Fimi7xY+J86R06WHojLo3yGzAgQCswZmD4CFf6NcBWDcb6l6kx5vk/AdzHIkVEZH4aikUL/fn3zq5qbE25oOg6pT7F7Pp4zdHOAEKnIRS8tvP8tvvVRkUCrjBxz/Kx6Ne1YOD+fkIMRk/MgIWeKZZzZOYx4VrC0vqYiM+PcKWbNdt1kNoTHOeL06XZeSE6WPZ3VB1QIDAQABo0IwQDAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBRPX3shmtgajnR4ly5t9VYB66ufGDAOBgNVHQ8BAf8EBAMCAoQwDQYJKoZIhvcNAQELBQADggEBAHtKpX70WU4uXOMjbFKj0e9HMXyCrdcX6TuYiMFqqlOGWM4yghSM8Bd0HkKcirm4DUoC+1dDMzXMZ+tbntavPt1xG0eRFjeocP+kIYTMQEG2LDM5HQ+Z7bdcwlxnuYOZQfpgKAfYbQ8Cxu38sB6q82I+5NJ0w0VXuG7nUZ1RD+rkXaeMYHNoibAtKBoTWrCaFWGV0E55OM+H0ckcHKUUnNXJOyZ+zEOzPFY5iuYIUmn1LfR1P0SLgIMfiooNC5ZuR/wLdbtyKtor2vzz7niEiewz+aPvfuPnWe/vMtQrfS37/yEhCozFnbIps/+S2Ay78mNBDuOAA9fg5yrnOmjABCU="]},{"alg":"RS256","kty":"RSA","use":"sig","n":"0KDpAuJZyDwPg9CfKi0R3QwDROyH0rvd39lmAoqQNqtYPghDToxFMDLpul0QHttbofHPJMKrPfeEFEOvw7KJgelCHZmckVKaz0e4tfu_2Uvw2kFljCmJGfspUU3mXxLyEea9Ef9JqUru6L8f_0_JIDMT3dceqU5ZqbG8u6-HRgRQ5Jqc_fF29Xyw3gxNP_Q46nsp_0yE68UZE1iPy1om0mpu8mpsY1-Nbvm51C8i4_tFQHdUXbhF4cjAoR0gZFNkzr7FCrL4On0hKeLcvxIHD17SxaBsTuCBGd35g7TmXsA4hSimD9taRHA-SkXh558JG5dr-YV9x80qjeSAvTyjcQ","e":"AQAB","kid":"v2HFn4VqJB-U4vtQRJ3Ql","x5t":"AhUBZjtsFdx7C1PFtWAJ756bo5k","x5c":["MIIDDTCCAfWgAwIBAgIJSSFLkuG8uAM8MA0GCSqGSIb3DQEBCwUAMCQxIjAgBgNVBAMTGWRldi1kdXp5YXlrNC5ldS5hdXRoMC5jb20wHhcNMjEwNjEzMDcxMTQ2WhcNMzUwMjIwMDcxMTQ2WjAkMSIwIAYDVQQDExlkZXYtZHV6eWF5azQuZXUuYXV0aDAuY29tMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0KDpAuJZyDwPg9CfKi0R3QwDROyH0rvd39lmAoqQNqtYPghDToxFMDLpul0QHttbofHPJMKrPfeEFEOvw7KJgelCHZmckVKaz0e4tfu/2Uvw2kFljCmJGfspUU3mXxLyEea9Ef9JqUru6L8f/0/JIDMT3dceqU5ZqbG8u6+HRgRQ5Jqc/fF29Xyw3gxNP/Q46nsp/0yE68UZE1iPy1om0mpu8mpsY1+Nbvm51C8i4/tFQHdUXbhF4cjAoR0gZFNkzr7FCrL4On0hKeLcvxIHD17SxaBsTuCBGd35g7TmXsA4hSimD9taRHA+SkXh558JG5dr+YV9x80qjeSAvTyjcQIDAQABo0IwQDAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBSEkRwvkyYzzzY/jPd1n7/1VRQNdzAOBgNVHQ8BAf8EBAMCAoQwDQYJKoZIhvcNAQELBQADggEBAGtdl7QwzpaWZjbmd6UINAIlpuWIo2v4EJD9kGan/tUZTiUdBaJVwFHOkLRsbZHc5PmBB5IryjOcrqsmKvFdo6wUZA92qTuQVZrOTea07msOKSWE6yRUh1/VCXH2+vAiB9A4DFZ23WpZikBR+DmiD8NGwVgAwWw9jM6pe7ODY+qxFXGjQdTCHcDdbqG2160nKEHCBvjR1Sc/F0pzHPv8CBJCyGAPTCXX42sKZI92pPzdKSmNNijCuIEYLsjzKVxaUuwEqIshk3mYeu6im4VmXXFj+MlyMsusVWi2py7fGFadamzyiV/bxZe+4xzzrRG1Kow/WnVEizfTdEzFXO6YikE="]}]}"#;

    fn generate_oidc_discovery_content(port: &str) -> Json<serde_json::Value> {
        let string = format!("http://{}/realms/master/protocol/openid-connect/cert", port);
        Json(serde_json::json!({
             "jwks_uri": string
        }))
    }

    fn setup_jwks_server() -> Rapina {
        Rapina::new()
            .with_introspection(false)
            .router(Router::new().route(
                http::Method::GET,
                "/realms/master/protocol/openid-connect/cert",
                |_, _, _| async { AUTH0_SAMPLE_JWKS },
            ))
    }

    fn setup_jwks_server_oidc_discovery() -> Rapina {
        Rapina::new().with_introspection(false).router(
            Router::new()
                .route(
                    http::Method::GET,
                    "/realms/master/protocol/openid-connect/cert",
                    |_, _, _| async { AUTH0_SAMPLE_JWKS },
                )
                .route(
                    http::Method::GET,
                    "/realms/master/.well-known/openid-configuration",
                    |req, _, _| async move {
                        //host header includes 127.0.0.1 and the test server port, e.g. "host": "127.0.0.1:49222"
                        let host_header =
                            req.headers().get(header::HOST).unwrap().to_str().unwrap();
                        generate_oidc_discovery_content(host_header)
                    },
                ),
        )
    }

    fn setup_jwks_client(addr: SocketAddr) -> JwksClient {
        let jwks_url = format!("http://{}/realms/master/protocol/openid-connect/cert", addr);
        direct_for_test(jwks_url, "* * * * * */5".to_string())
    }

    fn setup_jwks_client_oidc_discovery(addr: SocketAddr) -> JwksClient {
        let discovery_url = format!(
            "http://{}/realms/master/.well-known/openid-configuration",
            addr
        );
        oidc_for_test(discovery_url, "* * * * * */5".to_string())
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor() {
        let authorization_header = format!("Bearer {}", TEST_TOKEN);
        let (parts, _) = TestRequest::get("/")
            .header(AUTHORIZATION.as_str(), &authorization_header)
            .into_parts();

        let mut custom_validation = jwt::default_validation();
        custom_validation.set_audience(&[TEST_AUDIENCE]);
        //disable token expiration check because it will be run out at the time this test runs
        custom_validation.validate_exp = false;

        let jwks_server = TestClient::new(setup_jwks_server()).await;
        let jwks_client = setup_jwks_client(jwks_server.addr());

        let state = AppState::new().with(jwks_client).with(custom_validation);

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &Arc::new(state)).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor_oidc_discovery() {
        let authorization_header = format!("Bearer {}", TEST_TOKEN);
        let (parts, _) = TestRequest::get("/")
            .header(AUTHORIZATION.as_str(), &authorization_header)
            .into_parts();

        let mut custom_validation = jwt::default_validation();
        custom_validation.set_audience(&[TEST_AUDIENCE]);
        //disable token expiration check because it will be run out at the time this test runs
        custom_validation.validate_exp = false;

        let jwks_server = TestClient::new(setup_jwks_server_oidc_discovery()).await;
        let jwks_client = setup_jwks_client_oidc_discovery(jwks_server.addr());

        let state = AppState::new().with(jwks_client).with(custom_validation);

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &Arc::new(state)).await;

        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor_bad_token() {
        let (parts, _) = TestRequest::get("/")
            .header(AUTHORIZATION.as_str(), "Bearer xyz")
            .into_parts();

        let jwks_server = TestClient::new(setup_jwks_server()).await;
        let jwks_client = setup_jwks_client(jwks_server.addr());

        let state = AppState::new().with(jwks_client);

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &Arc::new(state)).await;

        let error = result.expect_err("Expected extraction to fail");
        assert_eq!(error.status(), 401);
        assert!(error.message().contains("invalid token"));
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor_missing_header() {
        let (parts, _) = TestRequest::get("/")
            .header("x-whatever", "hello")
            .into_parts();

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let error = result.expect_err("Expected extraction to fail");
        assert_eq!(error.status(), 401);
        assert!(error.message().contains("missing authorization header"));
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor_malformed_token() {
        let (parts, _) = TestRequest::get("/")
            .header(AUTHORIZATION.as_str(), "eyXhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiYWRtaW4iOnRydWUsImlhdCI6MTUxNjIzOTAyMn0.KMUFsIDTnFmyG3nMiGM6H9FNFUROf3wh7SmqJp-QV30")
            .into_parts();

        let jwks_server = TestClient::new(setup_jwks_server()).await;
        let jwks_client = setup_jwks_client(jwks_server.addr());

        let state = AppState::new().with(jwks_client);

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &Arc::new(state)).await;

        let error = result.expect_err("Expected extraction to fail");
        assert_eq!(error.status(), 401);
        assert!(error.message().contains("token header validation failed"));
    }

    #[tokio::test]
    async fn test_jsonwebtoken_extractor_invalid_jwks_configuration() {
        let (parts, _) = TestRequest::get("/")
            .header(AUTHORIZATION.as_str(), "eyXhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.eyJzdWIiOiIxMjM0NTY3ODkwIiwibmFtZSI6IkpvaG4gRG9lIiwiYWRtaW4iOnRydWUsImlhdCI6MTUxNjIzOTAyMn0.KMUFsIDTnFmyG3nMiGM6H9FNFUROf3wh7SmqJp-QV30")
            .into_parts();

        let result: Result<JsonWebToken<DefaultClaims>, Error> =
            JsonWebToken::from_request_parts(&parts, &empty_params(), &empty_state()).await;

        let error = result.expect_err("Expected extraction to fail");
        assert_eq!(error.status(), 500);
        assert!(error.message().contains("internal authentication error"));
    }

    #[test]
    #[should_panic(
        expected = "JWT misconfiguration: validate_aud is enabled but set_audience() was never called"
    )]
    fn test_jsonwebtoken_new_aud_misconfiguration() {
        let jwks: JwkSet = serde_json::from_str(AUTH0_SAMPLE_JWKS).unwrap();

        // validate_aud=true but no audience set via set_audience() — panics to fail fast
        let misconfigured_validation = jwt::default_validation();
        assert!(misconfigured_validation.validate_aud);
        assert!(misconfigured_validation.aud.is_none());

        let _: Result<JsonWebToken<DefaultClaims>, Error> = JsonWebToken::new(
            jwks,
            Some(&misconfigured_validation),
            TEST_TOKEN.to_string(),
        );
    }
}
