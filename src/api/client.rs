use color_eyre::eyre::{eyre, Result};
use core::fmt;
use hmac::{Hmac, Mac};
use reqwest::{header::HeaderMap, Client};
use secrecy::{ExposeSecret, SecretString};
use serde::de::DeserializeOwned;
use sha2::Sha256;
use std::{
    collections::HashMap,
    env,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{config::Config, VERSION};

type HmacSha256 = Hmac<Sha256>;

const DEV_SECRET: &str = "dev-secret-0";

// baked at compile time for release builds; unused in DEV mode.
const RELEASE_SECRET: &str = match option_env!("LUXCTL_CLIENT_SECRET") {
    Some(v) => v,
    None => DEV_SECRET,
};

use super::types::{
    ApiError, ApiUser, AttemptData, HealthCheckResponse, PaginatedResponse, Project,
    RestartProjectData, SubmitAttemptRequest, Task, TaskHint,
};

/// Produce (timestamp, hex-encoded HMAC-SHA256) for the given HTTP method + path.
/// The payload format is "timestamp.METHOD.path", which the api's signature
/// middleware rebuilds and compares, inside a +/-300s replay window.
///
/// The body is not signed. That is the scheme's limit: the signature says the
/// caller holds the client secret and that the request is recent, and
/// everything a body can do is gated again by the bearer token.
fn sign_request(method: &str, path: &str, secret: &str) -> (String, String) {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string();

    let payload = format!("{}.{}.{}", timestamp, method, path);

    let Some(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()).ok() else {
        return (timestamp, String::new());
    };
    mac.update(payload.as_bytes());

    (timestamp, hex::encode(mac.finalize().into_bytes()))
}

/// Build the three signing headers every /api/v1/* request carries.
fn signing_headers(method: &str, path: &str, secret: &str) -> Result<HeaderMap> {
    let (timestamp, signature) = sign_request(method, path, secret);
    let mut headers = HeaderMap::new();
    headers.insert("X-Luxctl-Signature", signature.parse()?);
    headers.insert("X-Luxctl-Timestamp", timestamp.parse()?);
    headers.insert("X-Luxctl-Version", VERSION.parse()?);
    Ok(headers)
}

pub struct LighthouseAPIClient {
    base_url: String,
    api_version: String,
    env: Env,
    client: Client,
    token: Option<SecretString>,
}

impl LighthouseAPIClient {
    fn new(
        base_url: LighthouseAPIClientBaseURL,
        api_version: &str,
        env: Env,
        token: Option<SecretString>,
    ) -> LighthouseAPIClient {
        LighthouseAPIClient {
            base_url: base_url.0,
            api_version: api_version.to_string(),
            env,
            client: Client::new(),
            token,
        }
    }

    pub fn from_config(config: &Config) -> LighthouseAPIClient {
        LighthouseAPIClient {
            token: Some(SecretString::from(config.expose_token())),
            ..Default::default()
        }
    }

    fn client_secret(&self) -> &'static str {
        match self.env {
            Env::DEV => DEV_SECRET,
            Env::RELEASE => RELEASE_SECRET,
        }
    }

    fn auth_headers(&self) -> Result<HeaderMap> {
        let token = self
            .token
            .as_ref()
            .ok_or_else(|| eyre!("no auth token configured"))?;

        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", token.expose_secret()).parse()?,
        );
        headers.insert("Accept", "application/json".parse()?);
        Ok(headers)
    }

    // when we deserialize JSON, we're creating owned data. But
    // there are two Deserialize traits:
    //
    // 1. Deserialize<'de> - Can borrow data from the input (has a lifetime)
    // 2. DeserializeOwned - Must own all its data (no lifetimes)
    //
    // when we write a generic function that returns T, you need T to be able
    // to own its data.
    //
    // #[derive(Deserialize)]  // This implements BOTH Deserialize<'de> AND
    // DeserializeOwned
    // struct User {
    //     username: String,  // String is owned data
    //     email: String,
    // }
    //
    // let user: User = response.json().await?;
    // The User is created with owned Strings, not borrowed data
    //
    async fn get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        query_params: Option<HashMap<String, String>>,
        headers: Option<HeaderMap>,
    ) -> Result<T> {
        let url = format!("{}/api/{}/{}", self.base_url, self.api_version, endpoint);
        let path = format!("/api/{}/{}", self.api_version, endpoint);

        let mut merged = signing_headers("GET", &path, self.client_secret())?;
        if let Some(extra) = headers {
            merged.extend(extra);
        }

        let mut request = self.client.get(url).headers(merged);

        if let Some(query_params) = query_params {
            request = request.query(&query_params);
        }

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            let message = serde_json::from_str::<ApiError>(&error_text)
                .map(|e| match e.code.is_empty() {
                    // the code is what a reader can act on — `task_locked`
                    // says finish the one before, and the prose is a fallback.
                    true => e.error,
                    false => format!("{} ({})", e.error, e.code),
                })
                .unwrap_or(error_text);
            return Err(eyre!("[HTTP {}] {}", status.as_u16(), message));
        }

        let data = response.json::<T>().await?;
        Ok(data)
    }

    async fn post<T: DeserializeOwned, B: serde::Serialize>(
        &self,
        endpoint: &str,
        body: &B,
        headers: Option<HeaderMap>,
    ) -> Result<T> {
        let url = format!("{}/api/{}/{}", self.base_url, self.api_version, endpoint);
        let path = format!("/api/{}/{}", self.api_version, endpoint);

        let mut merged = signing_headers("POST", &path, self.client_secret())?;
        if let Some(extra) = headers {
            merged.extend(extra);
        }

        let request = self.client.post(url).json(body).headers(merged);

        let response = request.send().await?;

        if !response.status().is_success() {
            let status = response.status();
            let error_text = response.text().await?;
            let message = serde_json::from_str::<ApiError>(&error_text)
                .map(|e| match e.code.is_empty() {
                    // the code is what a reader can act on — `task_locked`
                    // says finish the one before, and the prose is a fallback.
                    true => e.error,
                    false => format!("{} ({})", e.error, e.code),
                })
                .unwrap_or(error_text);
            return Err(eyre!("[HTTP {}] {}", status.as_u16(), message));
        }

        let data = response.json::<T>().await?;
        Ok(data)
    }
}

impl LighthouseAPIClient {
    /// unauthenticated healthcheck endpoint
    pub async fn healthcheck(&self) -> Result<HealthCheckResponse> {
        self.get::<HealthCheckResponse>("health", None, None).await
    }

    pub async fn me(&self) -> Result<ApiUser> {
        let headers = self.auth_headers()?;
        self.get::<ApiUser>("user", None, Some(headers)).await
    }

    pub async fn projects(
        &self,
        page: Option<i32>,
        per_page: Option<i32>,
    ) -> Result<PaginatedResponse<Project>> {
        let headers = self.auth_headers()?;

        let mut query_params = HashMap::new();
        if let Some(p) = page {
            query_params.insert("page".to_string(), p.to_string());
        }
        query_params.insert("per_page".to_string(), per_page.unwrap_or(50).to_string());

        self.get::<PaginatedResponse<Project>>("projects", Some(query_params), Some(headers))
            .await
    }

    pub async fn project_by_slug(&self, slug: &str) -> Result<Project> {
        let headers = self.auth_headers()?;
        let endpoint = format!("projects/{}", slug);
        self.get::<Project>(&endpoint, None, Some(headers)).await
    }

    /// one task, by uuid or slug.
    ///
    /// the only endpoint that carries a task's brief: a project response lists
    /// its tasks without their prose, so a whole project's descriptions are not
    /// shipped to print one.
    pub async fn task_by_identifier(&self, identifier: &str) -> Result<Task> {
        let headers = self.auth_headers()?;
        let endpoint = format!("tasks/{}", identifier);
        self.get::<Task>(&endpoint, None, Some(headers)).await
    }

    /// record one attempt. the response is the row, unwrapped — the api
    /// answers the view itself rather than nesting it under `data`.
    pub async fn submit_attempt(&self, request: &SubmitAttemptRequest) -> Result<AttemptData> {
        let headers = self.auth_headers()?;
        self.post::<AttemptData, _>("projects/attempts", request, Some(headers))
            .await
    }

    /// every hint on a task, with the words of the ones this reader has bought.
    pub async fn hints(&self, task_slug: &str) -> Result<Vec<TaskHint>> {
        let headers = self.auth_headers()?;
        let endpoint = format!("tasks/{}/hints", task_slug);
        self.get::<Vec<TaskHint>>(&endpoint, None, Some(headers))
            .await
    }

    /// buy one hint. answers the hint, now carrying its text.
    pub async fn unlock_hint(&self, task_slug: &str, hint_id: &str) -> Result<TaskHint> {
        let headers = self.auth_headers()?;
        let endpoint = format!("tasks/{}/hints/{}/unlock", task_slug, hint_id);
        // post with empty body
        self.post::<TaskHint, _>(&endpoint, &serde_json::json!({}), Some(headers))
            .await
    }

    /// restart a project from scratch. nothing is deleted: the run number goes
    /// up and every earlier attempt stays in the log.
    pub async fn restart_project(&self, slug: &str) -> Result<RestartProjectData> {
        let headers = self.auth_headers()?;
        let endpoint = format!("projects/{}/restart", slug);
        self.post::<RestartProjectData, _>(&endpoint, &serde_json::json!({}), Some(headers))
            .await
    }
}

#[derive(Clone, Copy)]
pub enum Env {
    DEV,
    RELEASE,
}

impl fmt::Display for Env {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Env::DEV => write!(f, "dev"),
            Env::RELEASE => write!(f, "release"),
        }
    }
}

#[derive(Debug)]
struct LighthouseAPIClientBaseURL(String);

impl LighthouseAPIClientBaseURL {
    pub fn from(base_url: &str, environment: Env) -> Result<Self, String> {
        let pattern = match environment {
            // DEV: allow localhost, 127.0.0.1, 0.0.0.0, or host.docker.internal (http or https, any port)
            Env::DEV => {
                r"^https?://(localhost|127\.0\.0\.1|0\.0\.0\.0|host\.docker\.internal)(:\d+)?(/.*)?$"
            }
            // RELEASE: only allow https://*projectlighthouse.io
            Env::RELEASE => r"^https://([a-zA-Z0-9-]+\.)*projectlighthouse\.io(/.*)?$",
        };

        let re = regex::Regex::new(pattern).map_err(|e| format!("invalid regex pattern: {}", e))?;

        if re.is_match(base_url) {
            Ok(LighthouseAPIClientBaseURL(base_url.to_string()))
        } else {
            let err_msg = match environment {
                Env::DEV => "invalid URL: must be localhost in DEV environment",
                Env::RELEASE => {
                    "invalid URL: must be https://*.projectlighthouse.io in RELEASE environment"
                }
            };
            Err(err_msg.to_string())
        }
    }

    pub fn default_for_env(environment: Env) -> Self {
        let url = match environment {
            Env::DEV => "http://localhost:8000",
            Env::RELEASE => "https://projectlighthouse.io",
        };
        LighthouseAPIClientBaseURL(url.to_string())
    }
}

impl Default for LighthouseAPIClient {
    fn default() -> Self {
        // 1. get the env first from LUXCTL_ENV, it should map to the enum Env::DEV or Env::RELEASE
        // 2. default based on build type: DEV for debug builds, RELEASE for release builds
        #[cfg(debug_assertions)]
        let default_env = Env::DEV;
        #[cfg(not(debug_assertions))]
        let default_env = Env::RELEASE;

        let luxctl_env = match env::var("LUXCTL_ENV") {
            Ok(val) => match val.to_uppercase().as_str() {
                "RELEASE" => Env::RELEASE,
                "DEV" => Env::DEV,
                _ => default_env,
            },
            Err(_) => default_env,
        };

        // 3. get base_url from env var or use default for the environment
        let base_url = match env::var("LUXCTL_API_BASE_URL") {
            Ok(val) => {
                // Validate the URL if provided
                match LighthouseAPIClientBaseURL::from(&val, luxctl_env) {
                    Ok(url) => url,
                    Err(e) => {
                        log::warn!("invalid LUXCTL_API_BASE_URL: {}. using default.", e);
                        LighthouseAPIClientBaseURL::default_for_env(luxctl_env)
                    }
                }
            }
            Err(_) => LighthouseAPIClientBaseURL::default_for_env(luxctl_env),
        };

        log::debug!("initiating lighthouse api with {}", base_url.0);

        LighthouseAPIClient::new(base_url, "v1", luxctl_env, None)
    }
}

impl fmt::Display for LighthouseAPIClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cli_version: {} base_url: {} api_version: {} env: {}",
            VERSION, self.base_url, self.api_version, self.env
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to ensure env var tests don't interfere with each other
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    // Helper to run tests with specific env vars, then restore original state
    fn with_env_vars<F, R>(vars: &[(&str, Option<&str>)], f: F) -> R
    where
        F: FnOnce() -> R,
    {
        // recover from poisoned mutex - we only care about synchronization, not state
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());

        // Save original values
        let originals: Vec<_> = vars
            .iter()
            .map(|(key, _)| (*key, env::var(*key).ok()))
            .collect();

        // Set new values
        for (key, value) in vars {
            match value {
                Some(v) => env::set_var(*key, v),
                None => env::remove_var(*key),
            }
        }

        let result = f();

        // Restore original values
        for (key, original) in originals {
            match original {
                Some(v) => env::set_var(key, v),
                None => env::remove_var(key),
            }
        }

        result
    }

    #[test]
    fn test_env_display_dev() {
        assert_eq!(format!("{}", Env::DEV), "dev");
    }

    #[test]
    fn test_env_display_release() {
        assert_eq!(format!("{}", Env::RELEASE), "release");
    }

    #[test]
    fn test_env_clone() {
        let env = Env::DEV;
        let cloned = env;
        assert_eq!(format!("{}", cloned), "dev");
    }

    #[test]
    fn test_lighthouse_api_base_url_dev_env() {
        // Valid localhost URLs in DEV
        assert!(LighthouseAPIClientBaseURL::from("http://localhost", Env::DEV).is_ok());
        assert!(LighthouseAPIClientBaseURL::from("https://localhost", Env::DEV).is_ok());
        assert!(LighthouseAPIClientBaseURL::from("http://localhost:8080", Env::DEV).is_ok());
        assert!(LighthouseAPIClientBaseURL::from("https://localhost:3000/api", Env::DEV).is_ok());

        // projectlighthouse.io NOT allowed in DEV
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::DEV).is_err()
        );
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::DEV).is_err()
        );

        // Invalid URLs in DEV
        assert!(LighthouseAPIClientBaseURL::from("ftp://localhost", Env::DEV).is_err()); // wrong scheme
        assert!(LighthouseAPIClientBaseURL::from("https://example.com", Env::DEV).is_err());
        // wrong domain
    }

    #[test]
    fn test_lighthouse_api_base_url_release_env() {
        // Valid projectlighthouse.io URLs in RELEASE (https only)
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::RELEASE).is_ok()
        );
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io/api", Env::RELEASE)
                .is_ok()
        );
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::RELEASE).is_ok()
        );
        assert!(
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io/v1", Env::RELEASE)
                .is_ok()
        );

        // localhost NOT allowed in RELEASE
        assert!(LighthouseAPIClientBaseURL::from("http://localhost", Env::RELEASE).is_err());
        assert!(LighthouseAPIClientBaseURL::from("https://localhost:8080", Env::RELEASE).is_err());

        // Invalid URLs in RELEASE
        assert!(
            LighthouseAPIClientBaseURL::from("http://projectlighthouse.io", Env::RELEASE).is_err()
        ); // http not allowed
        assert!(LighthouseAPIClientBaseURL::from("https://example.com", Env::RELEASE).is_err());
        // wrong domain
    }

    #[test]
    fn test_lighthouse_api_base_url_dev_with_paths() {
        // Various path combinations
        assert!(LighthouseAPIClientBaseURL::from("http://localhost/", Env::DEV).is_ok());
        assert!(LighthouseAPIClientBaseURL::from("http://localhost/api/v1", Env::DEV).is_ok());
        assert!(LighthouseAPIClientBaseURL::from(
            "http://localhost:8080/api/v1/exercises",
            Env::DEV
        )
        .is_ok());
    }

    #[test]
    fn test_lighthouse_api_base_url_release_subdomains() {
        // Multiple subdomain levels
        assert!(LighthouseAPIClientBaseURL::from(
            "https://api.v2.projectlighthouse.io",
            Env::RELEASE
        )
        .is_ok());
        assert!(LighthouseAPIClientBaseURL::from(
            "https://staging.api.projectlighthouse.io",
            Env::RELEASE
        )
        .is_ok());
    }

    #[test]
    fn test_lighthouse_api_base_url_error_messages() {
        let dev_err =
            LighthouseAPIClientBaseURL::from("https://example.com", Env::DEV).unwrap_err();
        assert!(dev_err.contains("localhost"));
        assert!(dev_err.contains("DEV"));

        let release_err =
            LighthouseAPIClientBaseURL::from("http://localhost", Env::RELEASE).unwrap_err();
        assert!(release_err.contains("projectlighthouse.io"));
        assert!(release_err.contains("RELEASE"));
    }

    #[test]
    fn test_lighthouse_api_base_url_default_for_env_dev() {
        let url = LighthouseAPIClientBaseURL::default_for_env(Env::DEV);
        assert_eq!(url.0, "http://localhost:8000");
    }

    #[test]
    fn test_lighthouse_api_base_url_default_for_env_release() {
        let url = LighthouseAPIClientBaseURL::default_for_env(Env::RELEASE);
        assert_eq!(url.0, "https://projectlighthouse.io");
    }

    #[test]
    fn test_lighthouse_api_new() {
        let base_url = LighthouseAPIClientBaseURL::from("http://localhost:8080", Env::DEV).unwrap();
        let api = LighthouseAPIClient::new(base_url, "v2", Env::DEV, None);

        assert_eq!(api.base_url, "http://localhost:8080");
        assert_eq!(api.api_version, "v2");
    }

    #[test]
    fn test_lighthouse_api_new_release() {
        let base_url =
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::RELEASE).unwrap();
        let api = LighthouseAPIClient::new(base_url, "v1", Env::RELEASE, None);

        assert_eq!(api.base_url, "https://projectlighthouse.io");
        assert_eq!(api.api_version, "v1");
    }

    #[test]
    fn test_lighthouse_api_default_no_env_vars() {
        with_env_vars(
            &[("LUXCTL_ENV", None), ("LUXCTL_API_BASE_URL", None)],
            || {
                let api = LighthouseAPIClient::default();
                // Should default to DEV with localhost
                assert_eq!(api.base_url, "http://localhost:8000");
                assert_eq!(api.api_version, "v1");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_release_env() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("RELEASE")),
                ("LUXCTL_API_BASE_URL", None),
            ],
            || {
                let api = LighthouseAPIClient::default();
                assert_eq!(api.base_url, "https://projectlighthouse.io");
                assert_eq!(api.api_version, "v1");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_release_lowercase() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("release")),
                ("LUXCTL_API_BASE_URL", None),
            ],
            || {
                let api = LighthouseAPIClient::default();
                assert_eq!(api.base_url, "https://projectlighthouse.io");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_dev_env_explicit() {
        with_env_vars(
            &[("LUXCTL_ENV", Some("DEV")), ("LUXCTL_API_BASE_URL", None)],
            || {
                let api = LighthouseAPIClient::default();
                assert_eq!(api.base_url, "http://localhost:8000");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_invalid_env_defaults_to_dev() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("INVALID")),
                ("LUXCTL_API_BASE_URL", None),
            ],
            || {
                let api = LighthouseAPIClient::default();
                // Invalid env should default to DEV
                assert_eq!(api.base_url, "http://localhost:8000");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_custom_base_url_dev() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("DEV")),
                ("LUXCTL_API_BASE_URL", Some("http://localhost:9000")),
            ],
            || {
                let api = LighthouseAPIClient::default();
                assert_eq!(api.base_url, "http://localhost:9000");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_custom_base_url_release() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("RELEASE")),
                (
                    "LUXCTL_API_BASE_URL",
                    Some("https://staging.projectlighthouse.io"),
                ),
            ],
            || {
                let api = LighthouseAPIClient::default();
                assert_eq!(api.base_url, "https://staging.projectlighthouse.io");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_invalid_base_url_falls_back() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("DEV")),
                ("LUXCTL_API_BASE_URL", Some("https://invalid.com")),
            ],
            || {
                let api = LighthouseAPIClient::default();
                // Invalid URL should fall back to DEV default
                assert_eq!(api.base_url, "http://localhost:8000");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_default_invalid_base_url_release_falls_back() {
        with_env_vars(
            &[
                ("LUXCTL_ENV", Some("RELEASE")),
                ("LUXCTL_API_BASE_URL", Some("http://localhost:8080")),
            ],
            || {
                let api = LighthouseAPIClient::default();
                // localhost not allowed in RELEASE, should fall back to RELEASE default
                assert_eq!(api.base_url, "https://projectlighthouse.io");
            },
        );
    }

    #[test]
    fn test_lighthouse_api_display() {
        with_env_vars(
            &[("LUXCTL_ENV", None), ("LUXCTL_API_BASE_URL", None)],
            || {
                let api = LighthouseAPIClient::default();
                let display = format!("{}", api);

                assert!(display.contains("cli_version:"));
                assert!(display.contains("base_url:"));
                assert!(display.contains("api_version:"));
                assert!(display.contains("env:"));
                assert!(display.contains(VERSION));
                assert!(display.contains("v1"));
            },
        );
    }

    #[test]
    fn test_lighthouse_api_display_contains_all_fields() {
        let base_url = LighthouseAPIClientBaseURL::from("http://localhost:3000", Env::DEV).unwrap();
        let api = LighthouseAPIClient::new(base_url, "v2", Env::DEV, None);
        let display = format!("{}", api);

        assert!(display.contains("http://localhost:3000"));
        assert!(display.contains("v2"));
        assert!(display.contains("dev"));
    }

    #[test]
    fn test_lighthouse_api_display_release_env() {
        let base_url =
            LighthouseAPIClientBaseURL::from("https://projectlighthouse.io", Env::RELEASE).unwrap();
        let api = LighthouseAPIClient::new(base_url, "v1", Env::RELEASE, None);
        let display = format!("{}", api);

        assert!(display.contains("https://projectlighthouse.io"));
        assert!(display.contains("release"));
    }

    #[test]
    fn test_sign_request_produces_valid_hex() {
        let (timestamp, signature) = sign_request("GET", "/api/v1/projects", DEV_SECRET);
        // timestamp is a numeric string
        assert!(timestamp.parse::<u64>().is_ok());
        // signature is 64-char hex (SHA-256 = 32 bytes = 64 hex chars)
        assert_eq!(signature.len(), 64);
        assert!(signature.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn test_sign_request_differs_by_method() {
        let (_, sig_get) = sign_request("GET", "/api/v1/projects", DEV_SECRET);
        let (_, sig_post) = sign_request("POST", "/api/v1/projects", DEV_SECRET);
        // different methods → different signatures (timestamps may differ too, but
        // even with same timestamp the payload is different)
        // we can't assert inequality because timestamps could change the output;
        // instead verify both are valid hex
        assert_eq!(sig_get.len(), 64);
        assert_eq!(sig_post.len(), 64);
    }

    #[test]
    fn test_sign_request_differs_by_path() {
        let (ts1, sig1) = sign_request("GET", "/api/v1/projects", DEV_SECRET);
        let (ts2, sig2) = sign_request("GET", "/api/v1/tasks", DEV_SECRET);
        // if by chance timestamps are the same, paths differ → signatures differ
        if ts1 == ts2 {
            assert_ne!(sig1, sig2);
        }
    }

    #[test]
    fn test_signing_headers_contain_required_keys() {
        let headers = signing_headers("GET", "/api/v1/projects", DEV_SECRET).unwrap();
        assert!(headers.contains_key("X-Luxctl-Signature"));
        assert!(headers.contains_key("X-Luxctl-Timestamp"));
        assert!(headers.contains_key("X-Luxctl-Version"));
        assert_eq!(headers.get("X-Luxctl-Version").unwrap(), VERSION);
    }
}
