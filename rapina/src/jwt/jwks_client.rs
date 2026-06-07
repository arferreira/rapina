use crate::error::Error;
use bytes::{Buf, Bytes};
use http::Uri;
use http_body_util::{BodyExt, Empty};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::client::legacy::Client;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use jsonwebtoken::Validation;
use jsonwebtoken::jwk::JwkSet;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use tokio::sync::RwLock;

pub(crate) type HttpsClient = Client<HttpsConnector<HttpConnector>, Empty<Bytes>>;

#[derive(Clone)]
pub enum JwksClient {
    Direct {
        client: HttpsClient,
        jwks_url: String,
        refresh_schedule: String,
        cache: Arc<RwLock<Option<JwkSet>>>,
    },
    Oidc {
        client: HttpsClient,
        discovery_url: String,
        refresh_schedule: String,
        cache: Arc<RwLock<Option<JwkSet>>>,
    },
}

impl JwksClient {
    pub fn oidc(discovery_url: String, refresh_schedule: String) -> JwksClient {
        require_https_url(&discovery_url);

        Self::Oidc {
            client: build_https_client(),
            discovery_url,
            refresh_schedule,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn direct(jwks_url: String, refresh_schedule: String) -> JwksClient {
        require_https_url(&jwks_url);

        Self::Direct {
            client: build_https_client(),
            jwks_url,
            refresh_schedule,
            cache: Arc::new(RwLock::new(None)),
        }
    }

    pub fn refresh_schedule(&self) -> &str {
        match self {
            JwksClient::Oidc {
                refresh_schedule, ..
            } => refresh_schedule,
            JwksClient::Direct {
                refresh_schedule, ..
            } => refresh_schedule,
        }
    }

    pub async fn jwks_content(&self) -> Option<JwkSet> {
        match self {
            JwksClient::Oidc { cache, .. } => cache.read().await.clone(),
            JwksClient::Direct { cache, .. } => cache.read().await.clone(),
        }
    }

    fn cache(&self) -> &Arc<RwLock<Option<JwkSet>>> {
        match self {
            JwksClient::Oidc { cache, .. } => cache,
            JwksClient::Direct { cache, .. } => cache,
        }
    }

    async fn fetch_jwks_content(&self) -> Result<JwkSet, Error> {
        match self {
            JwksClient::Direct {
                client, jwks_url, ..
            } => fetch_json_content(client, jwks_url).await.map_err(|e| {
                Error::internal(format!("Failed to retrieve data from JWKS uri: {}", e))
            }),
            JwksClient::Oidc {
                client,
                discovery_url,
                ..
            } => {
                #[derive(Deserialize)]
                struct OidcConfig {
                    jwks_uri: String,
                }
                let oidc_config: OidcConfig = fetch_json_content(client, discovery_url)
                    .await
                    .map_err(|_| {
                        Error::internal("Failed to retrieve data from OIDC discovery endpoint")
                    })?;

                fetch_json_content(client, &oidc_config.jwks_uri)
                    .await
                    .map_err(|e| {
                        Error::internal(format!("Failed to retrieve data from JWKS uri: {}", e))
                    })
            }
        }
    }

    pub(crate) async fn refresh_jwks_cache(&self) -> Result<(), Error> {
        tracing::debug!("Refreshing JWKS cache");

        let content = self.fetch_jwks_content().await?;
        self.cache().write().await.replace(content);

        Ok(())
    }
}

pub fn default_validation() -> Validation {
    let mut validation = Validation::default();

    // account for 10 seconds of clock skew per default
    validation.leeway = 10;

    // enable aud (audience), exp (expiration) and nbf (not before) field validation
    validation.validate_aud = true;
    validation.validate_exp = true;
    validation.validate_nbf = true;

    // require aud to be present in every token — aligns with jsonwebtoken crate recommendation
    validation.required_spec_claims.insert("aud".to_string());

    validation
}

/// Creates a HTTP client in Release mode, with strict enforcement for HTTPS connections
fn build_https_client() -> HttpsClient {
    let builder = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("no native root CA certificates found")
        .https_only()
        .enable_all_versions()
        .build();
    Client::builder(TokioExecutor::new()).build(builder)
}

/// Requires the input `:url` to use HTTPS scheme.
///
/// # Panics
/// Panics if `:url` is not using a HTTPS scheme
fn require_https_url(url: &str) {
    let uri: Uri = url.parse().expect("Failed to parse URL");
    match uri.scheme_str() {
        Some("https") => (),
        Some(_) => {
            panic!(
                "Unsupported scheme was used for JWKS url. Must use HTTPS only (https://...). Fetching JWKS over plain HTTP allows network attackers to inject forged public keys and sign arbitrary tokens"
            );
        }
        None => {
            panic!("JWKS url is missing a scheme. Use an url that starts with https://");
        }
    }
}

async fn fetch_json_content<T: DeserializeOwned>(
    client: &HttpsClient,
    uri: &str,
) -> Result<T, Error> {
    let uri: Uri = uri
        .parse::<Uri>()
        .map_err(|e| Error::internal(format!("Invalid URI: {}", e)))?;

    let res = client
        .get(uri)
        .await
        .map_err(|e| Error::internal(format!("Failed to get data: {}", e)))?;

    let body = res
        .collect()
        .await
        .map_err(|e| Error::internal(format!("Body extractor failed: {}", e)))?
        .aggregate();

    let json: T = serde_json::from_reader(body.reader())
        .map_err(|e| Error::internal(format!("Failed parsing result to JSON: {}", e)))?;
    Ok(json)
}

/// Builds a `JwksClient` that accepts plain HTTP — for use in tests only.
#[cfg(test)]
pub(crate) fn direct_for_test(jwks_url: String, refresh_schedule: String) -> JwksClient {
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("no native root CA certificates found")
        .https_or_http()
        .enable_all_versions()
        .build();
    let client = Client::builder(TokioExecutor::new()).build(connector);
    JwksClient::Direct {
        client,
        jwks_url,
        refresh_schedule,
        cache: Arc::new(RwLock::new(None)),
    }
}

/// Builds an OIDC `JwksClient` that accepts plain HTTP — for use in tests only.
#[cfg(test)]
pub(crate) fn oidc_for_test(discovery_url: String, refresh_schedule: String) -> JwksClient {
    let connector = HttpsConnectorBuilder::new()
        .with_native_roots()
        .expect("no native root CA certificates found")
        .https_or_http()
        .enable_all_versions()
        .build();
    let client = Client::builder(TokioExecutor::new()).build(connector);
    JwksClient::Oidc {
        client,
        discovery_url,
        refresh_schedule,
        cache: Arc::new(RwLock::new(None)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Rapina;
    use crate::extract::Json;
    use crate::prelude::Router;
    use crate::testing::TestClient;
    use http::header;
    use std::net::SocketAddr;

    const AUTH0_SAMPLE_JWKS: &str = r#"{"keys":[{"alg":"RS256","kty":"RSA","use":"sig","n":"2V31IZF-EY2GxXQPI5OaEE--sezizPamNZDW9AjBE2cCErfufM312nT2jUsCnfjsXnh6Z_b-ncOMr97zIZkq1ofU7avemv8nX7NpKmoPBpVrMPprOax2-e3wt-bSfFLIHyghjFLKpkT0LOL_Fimi7xY-J86R06WHojLo3yGzAgQCswZmD4CFf6NcBWDcb6l6kx5vk_AdzHIkVEZH4aikUL_fn3zq5qbE25oOg6pT7F7Pp4zdHOAEKnIRS8tvP8tvvVRkUCrjBxz_Kx6Ne1YOD-fkIMRk_MgIWeKZZzZOYx4VrC0vqYiM-PcKWbNdt1kNoTHOeL06XZeSE6WPZ3VB1Q","e":"AQAB","kid":"1Z57d_i7TE6KTY57pKzDy","x5t":"1gA-aTE9VglLXZnrqvzwWhHsFdk","x5c":["MIIDDTCCAfWgAwIBAgIJHwhLfcIbNvmkMA0GCSqGSIb3DQEBCwUAMCQxIjAgBgNVBAMTGWRldi1kdXp5YXlrNC5ldS5hdXRoMC5jb20wHhcNMjEwNjEzMDcxMTQ1WhcNMzUwMjIwMDcxMTQ1WjAkMSIwIAYDVQQDExlkZXYtZHV6eWF5azQuZXUuYXV0aDAuY29tMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA2V31IZF+EY2GxXQPI5OaEE++sezizPamNZDW9AjBE2cCErfufM312nT2jUsCnfjsXnh6Z/b+ncOMr97zIZkq1ofU7avemv8nX7NpKmoPBpVrMPprOax2+e3wt+bSfFLIHyghjFLKpkT0LOL/Fimi7xY+J86R06WHojLo3yGzAgQCswZmD4CFf6NcBWDcb6l6kx5vk/AdzHIkVEZH4aikUL/fn3zq5qbE25oOg6pT7F7Pp4zdHOAEKnIRS8tvP8tvvVRkUCrjBxz/Kx6Ne1YOD+fkIMRk/MgIWeKZZzZOYx4VrC0vqYiM+PcKWbNdt1kNoTHOeL06XZeSE6WPZ3VB1QIDAQABo0IwQDAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBRPX3shmtgajnR4ly5t9VYB66ufGDAOBgNVHQ8BAf8EBAMCAoQwDQYJKoZIhvcNAQELBQADggEBAHtKpX70WU4uXOMjbFKj0e9HMXyCrdcX6TuYiMFqqlOGWM4yghSM8Bd0HkKcirm4DUoC+1dDMzXMZ+tbntavPt1xG0eRFjeocP+kIYTMQEG2LDM5HQ+Z7bdcwlxnuYOZQfpgKAfYbQ8Cxu38sB6q82I+5NJ0w0VXuG7nUZ1RD+rkXaeMYHNoibAtKBoTWrCaFWGV0E55OM+H0ckcHKUUnNXJOyZ+zEOzPFY5iuYIUmn1LfR1P0SLgIMfiooNC5ZuR/wLdbtyKtor2vzz7niEiewz+aPvfuPnWe/vMtQrfS37/yEhCozFnbIps/+S2Ay78mNBDuOAA9fg5yrnOmjABCU="]},{"alg":"RS256","kty":"RSA","use":"sig","n":"0KDpAuJZyDwPg9CfKi0R3QwDROyH0rvd39lmAoqQNqtYPghDToxFMDLpul0QHttbofHPJMKrPfeEFEOvw7KJgelCHZmckVKaz0e4tfu_2Uvw2kFljCmJGfspUU3mXxLyEea9Ef9JqUru6L8f_0_JIDMT3dceqU5ZqbG8u6-HRgRQ5Jqc_fF29Xyw3gxNP_Q46nsp_0yE68UZE1iPy1om0mpu8mpsY1-Nbvm51C8i4_tFQHdUXbhF4cjAoR0gZFNkzr7FCrL4On0hKeLcvxIHD17SxaBsTuCBGd35g7TmXsA4hSimD9taRHA-SkXh558JG5dr-YV9x80qjeSAvTyjcQ","e":"AQAB","kid":"v2HFn4VqJB-U4vtQRJ3Ql","x5t":"AhUBZjtsFdx7C1PFtWAJ756bo5k","x5c":["MIIDDTCCAfWgAwIBAgIJSSFLkuG8uAM8MA0GCSqGSIb3DQEBCwUAMCQxIjAgBgNVBAMTGWRldi1kdXp5YXlrNC5ldS5hdXRoMC5jb20wHhcNMjEwNjEzMDcxMTQ2WhcNMzUwMjIwMDcxMTQ2WjAkMSIwIAYDVQQDExlkZXYtZHV6eWF5azQuZXUuYXV0aDAuY29tMIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA0KDpAuJZyDwPg9CfKi0R3QwDROyH0rvd39lmAoqQNqtYPghDToxFMDLpul0QHttbofHPJMKrPfeEFEOvw7KJgelCHZmckVKaz0e4tfu/2Uvw2kFljCmJGfspUU3mXxLyEea9Ef9JqUru6L8f/0/JIDMT3dceqU5ZqbG8u6+HRgRQ5Jqc/fF29Xyw3gxNP/Q46nsp/0yE68UZE1iPy1om0mpu8mpsY1+Nbvm51C8i4/tFQHdUXbhF4cjAoR0gZFNkzr7FCrL4On0hKeLcvxIHD17SxaBsTuCBGd35g7TmXsA4hSimD9taRHA+SkXh558JG5dr+YV9x80qjeSAvTyjcQIDAQABo0IwQDAPBgNVHRMBAf8EBTADAQH/MB0GA1UdDgQWBBSEkRwvkyYzzzY/jPd1n7/1VRQNdzAOBgNVHQ8BAf8EBAMCAoQwDQYJKoZIhvcNAQELBQADggEBAGtdl7QwzpaWZjbmd6UINAIlpuWIo2v4EJD9kGan/tUZTiUdBaJVwFHOkLRsbZHc5PmBB5IryjOcrqsmKvFdo6wUZA92qTuQVZrOTea07msOKSWE6yRUh1/VCXH2+vAiB9A4DFZ23WpZikBR+DmiD8NGwVgAwWw9jM6pe7ODY+qxFXGjQdTCHcDdbqG2160nKEHCBvjR1Sc/F0pzHPv8CBJCyGAPTCXX42sKZI92pPzdKSmNNijCuIEYLsjzKVxaUuwEqIshk3mYeu6im4VmXXFj+MlyMsusVWi2py7fGFadamzyiV/bxZe+4xzzrRG1Kow/WnVEizfTdEzFXO6YikE="]}]}"#;

    fn generate_oidc_discovery_content(port: &str) -> Json<serde_json::Value> {
        let string = format!("http://{}/realms/master/protocol/openid-connect/cert", port);
        Json(serde_json::json!({
             "jwks_uri": string
        }))
    }

    fn setup_jwks_server_direct() -> Rapina {
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

    fn setup_jwks_client_direct(addr: SocketAddr) -> JwksClient {
        let jwks_url = format!("http://{}/realms/master/protocol/openid-connect/cert", addr);
        direct_for_test(jwks_url, "0 0 0 0 0 0".to_string())
    }

    fn setup_jwks_client_oidc_discovery(addr: SocketAddr) -> JwksClient {
        let discovery_url = format!(
            "http://{}/realms/master/.well-known/openid-configuration",
            addr
        );
        oidc_for_test(discovery_url, "0 0 0 0 0 0".to_string())
    }

    #[test]
    fn test_refresh_schedule_direct() {
        let client = JwksClient::direct(
            "https://example.com/jwks".to_string(),
            "0 */5 * * * *".to_string(),
        );
        assert_eq!(client.refresh_schedule(), "0 */5 * * * *");
    }

    #[test]
    fn test_refresh_schedule_oidc() {
        let client = JwksClient::oidc(
            "https://example.com/.well-known/openid-configuration".to_string(),
            "0 */10 * * * *".to_string(),
        );
        assert_eq!(client.refresh_schedule(), "0 */10 * * * *");
    }

    #[test]
    #[should_panic]
    fn test_invalid_https_scheme_panics_oidc() {
        let oidc_discovery_url =
            "http://example.com/realms/master/.well-known/openid-configuration";
        let refresh_schedule = "0 */5 * * * *";

        //this must panic because the scheme is http:// but Rapina only allows https:// scheme
        JwksClient::oidc(oidc_discovery_url.to_string(), refresh_schedule.to_string());
    }

    #[test]
    #[should_panic]
    fn test_invalid_https_scheme_panics_direct() {
        let jwks_url = "http://example.com/xyz";
        let refresh_schedule = "0 */5 * * * *";

        //this must panic because the scheme is http:// but Rapina only allows https:// scheme
        JwksClient::direct(jwks_url.to_string(), refresh_schedule.to_string());
    }

    #[tokio::test]
    async fn test_cache_empty_by_default_direct() {
        let client = JwksClient::direct(
            "https://example.com/jwks".to_string(),
            "0 */5 * * * *".to_string(),
        );
        assert!(client.jwks_content().await.is_none());
    }

    #[tokio::test]
    async fn test_cache_empty_by_default_oidc() {
        let client = JwksClient::oidc(
            "https://example.com/.well-known/openid-configuration".to_string(),
            "0 */5 * * * *".to_string(),
        );
        assert!(client.jwks_content().await.is_none());
    }

    #[tokio::test]
    async fn test_refresh_populates_cache_direct() {
        let server = TestClient::new(setup_jwks_server_direct()).await;
        let client = setup_jwks_client_direct(server.addr());

        assert!(client.jwks_content().await.is_none());

        client.refresh_jwks_cache().await.unwrap();

        let jwks = client.jwks_content().await;
        assert!(jwks.is_some());
        assert!(!jwks.unwrap().keys.is_empty());
    }

    #[tokio::test]
    async fn test_refresh_populates_cache_oidc() {
        let server = TestClient::new(setup_jwks_server_oidc_discovery()).await;
        let client = setup_jwks_client_oidc_discovery(server.addr());

        assert!(client.jwks_content().await.is_none());

        client.refresh_jwks_cache().await.unwrap();

        let jwks = client.jwks_content().await;
        assert!(jwks.is_some());
        assert!(!jwks.unwrap().keys.is_empty());
    }

    #[tokio::test]
    async fn test_cache_shared_across_clones_direct() {
        let server = TestClient::new(setup_jwks_server_direct()).await;
        let client = setup_jwks_client_direct(server.addr());
        let clone = client.clone();

        assert!(clone.jwks_content().await.is_none());

        client.refresh_jwks_cache().await.unwrap();

        // Clone sees the updated cache
        assert!(clone.jwks_content().await.is_some());
    }

    #[tokio::test]
    async fn test_cache_shared_across_clones_oidc() {
        let server = TestClient::new(setup_jwks_server_oidc_discovery()).await;
        let client = setup_jwks_client_oidc_discovery(server.addr());
        let clone = client.clone();

        assert!(clone.jwks_content().await.is_none());

        client.refresh_jwks_cache().await.unwrap();

        // Clone sees the updated cache
        assert!(clone.jwks_content().await.is_some());
    }

    #[tokio::test]
    async fn test_refresh_with_unreachable_server_direct() {
        //empty server
        let server = TestClient::new(Rapina::new()).await;
        let client = setup_jwks_client_direct(server.addr());

        let result = client.refresh_jwks_cache().await;
        assert!(result.is_err());
        assert!(client.jwks_content().await.is_none());
    }

    #[tokio::test]
    async fn test_refresh_with_unreachable_server_oidc() {
        //empty server
        let server = TestClient::new(Rapina::new()).await;
        let client = setup_jwks_client_oidc_discovery(server.addr());

        let result = client.refresh_jwks_cache().await;
        assert!(result.is_err());
        assert!(client.jwks_content().await.is_none());
    }
}
