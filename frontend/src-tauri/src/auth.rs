use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use keyring::Entry;
use once_cell::sync::Lazy;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use tauri::{App, Emitter, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

const REDIRECT_URI: &str = "meetingly://oauth/callback";
const SCOPES: &str = "openid profile email user:org:read";
const KEYCHAIN_SERVICE: &str = "meetingly-clerk-oauth";
const KEYCHAIN_ACCOUNT: &str = "session";
const CONNECTIONS_DEFAULT_URL: &str = "https://connections.fractals-solutions.com";

static CURRENT_SESSION: Lazy<RwLock<Option<AuthSession>>> = Lazy::new(|| RwLock::new(None));
static PENDING: Lazy<RwLock<Option<PendingAuthorization>>> = Lazy::new(|| RwLock::new(None));
static RECORDING_IDENTITY: Lazy<RwLock<Option<OperationIdentity>>> =
    Lazy::new(|| RwLock::new(None));
#[cfg(test)]
pub(crate) static AUTH_TEST_MUTEX: Lazy<std::sync::Mutex<()>> =
    Lazy::new(|| std::sync::Mutex::new(()));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationIdentity {
    pub clerk_org_id: String,
    pub user_id: String,
}

impl OperationIdentity {
    pub fn new(clerk_org_id: impl Into<String>, user_id: impl Into<String>) -> Self {
        Self {
            clerk_org_id: clerk_org_id.into(),
            user_id: user_id.into(),
        }
    }
}

pub struct RecordingIdentityGuard {
    identity: OperationIdentity,
    committed: bool,
}

impl RecordingIdentityGuard {
    pub fn identity(&self) -> &OperationIdentity {
        &self.identity
    }

    pub fn commit(mut self) {
        self.committed = true;
    }
}

impl Drop for RecordingIdentityGuard {
    fn drop(&mut self) {
        if !self.committed {
            clear_recording_identity();
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSession {
    pub user_id: String,
    pub clerk_org_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredTokens {
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct PendingAuthorization {
    state: String,
    verifier: String,
}

#[derive(Debug, Deserialize)]
struct OAuthMetadata {
    authorization_endpoint: String,
    token_endpoint: String,
    revocation_endpoint: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct AccessClaims {
    sub: String,
    org_id: String,
    aud: Option<serde_json::Value>,
    exp: i64,
    iss: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VerifiedSession {
    user_id: String,
    session_claims: serde_json::Value,
}

pub fn setup_deep_links<R: Runtime>(app: &App<R>) -> Result<()> {
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            let callback = url.clone();
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                match finish_authorization(callback).await {
                    Ok(session) => {
                        let _ = app.emit("auth-changed", Some(session));
                    }
                    Err(error) => {
                        log::error!("Clerk OAuth callback failed: {error:#}");
                        let _ = app.emit("auth-error", error.to_string());
                    }
                }
            });
        }
    });

    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    app.deep_link().register_all()?;
    Ok(())
}

#[tauri::command]
pub async fn auth_start_sign_in() -> std::result::Result<(), String> {
    ensure_organization_switching_allowed()?;
    let metadata = metadata().await.map_err(display_error)?;
    let verifier = random_urlsafe(32);
    let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    let state = random_urlsafe(24);
    let mut authorization_url =
        Url::parse(&metadata.authorization_endpoint).map_err(display_error)?;
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id().map_err(display_error)?)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "consent");
    *PENDING
        .write()
        .map_err(|_| "OAuth state lock is unavailable".to_string())? =
        Some(PendingAuthorization { state, verifier });
    crate::api::api::open_external_url(authorization_url.to_string()).await
}

#[tauri::command]
pub async fn auth_get_session() -> std::result::Result<Option<AuthSession>, String> {
    match access_token().await {
        Ok(_) => Ok(current_session()),
        Err(error) => {
            log::warn!("No usable Clerk session: {error:#}");
            Ok(None)
        }
    }
}

#[tauri::command]
pub async fn auth_sign_out() -> std::result::Result<(), String> {
    ensure_organization_switching_allowed()?;
    if let Ok(tokens) = load_tokens() {
        if let Ok(metadata) = metadata().await {
            if let Some(endpoint) = metadata.revocation_endpoint {
                let _ = reqwest::Client::new()
                    .post(endpoint)
                    .form(&[
                        ("token", tokens.refresh_token),
                        ("client_id", client_id().unwrap_or_default()),
                    ])
                    .send()
                    .await;
            }
        }
    }
    delete_tokens().map_err(display_error)?;
    set_current_session(None);
    Ok(())
}

#[tauri::command]
pub async fn auth_open_profile() -> std::result::Result<(), String> {
    open_portal("user").await
}

#[tauri::command]
pub async fn auth_open_organization() -> std::result::Result<(), String> {
    ensure_organization_switching_allowed()?;
    open_portal("organization").await
}

#[tauri::command]
pub fn auth_finish_recording_scope() -> std::result::Result<(), String> {
    let identity = require_current_operation_identity().map_err(display_error)?;
    if finish_recording_identity_if_matches(&identity) {
        Ok(())
    } else {
        Err("The retained recording belongs to a different Clerk user or organization.".to_string())
    }
}

pub async fn access_token() -> Result<String> {
    let mut tokens = load_tokens()?;
    if tokens.expires_at <= chrono::Utc::now().timestamp() + 60 {
        tokens = refresh_tokens(tokens).await?;
        save_tokens(&tokens)?;
    }
    let session = validate_access_token(&tokens.access_token).await?;
    set_current_session(Some(session));
    Ok(tokens.access_token)
}

pub fn require_clerk_org_id() -> Result<String> {
    if let Some(identity) = recording_identity() {
        return Ok(identity.clerk_org_id);
    }
    require_current_clerk_org_id()
}

pub fn require_current_clerk_org_id() -> Result<String> {
    current_session()
        .map(|session| session.clerk_org_id)
        .ok_or_else(|| anyhow!("A Clerk organization is required."))
}

pub fn require_user_id() -> Result<String> {
    if let Some(identity) = recording_identity() {
        return Ok(identity.user_id);
    }
    require_current_user_id()
}

pub fn require_current_user_id() -> Result<String> {
    current_session()
        .map(|session| session.user_id)
        .ok_or_else(|| anyhow!("A Clerk user is required."))
}

pub fn begin_recording_identity() -> Result<RecordingIdentityGuard> {
    begin_operation_identity()
}

pub fn begin_operation_identity() -> Result<RecordingIdentityGuard> {
    let session = current_session().context("A verified Clerk session is required to record.")?;
    let identity = OperationIdentity::new(session.clerk_org_id, session.user_id);
    let mut locked = RECORDING_IDENTITY
        .write()
        .map_err(|_| anyhow!("Recording identity lock is unavailable"))?;
    if locked.is_some() {
        return Err(anyhow!("A recording organization is already locked."));
    }
    *locked = Some(identity.clone());
    Ok(RecordingIdentityGuard {
        identity,
        committed: false,
    })
}

pub fn require_operation_identity() -> Result<OperationIdentity> {
    recording_identity()
        .or_else(current_operation_identity)
        .context("A Clerk organization is required.")
}

pub fn require_current_operation_identity() -> Result<OperationIdentity> {
    current_operation_identity().context("A Clerk organization is required.")
}

pub fn connections_url() -> String {
    std::env::var("MEETINGLY_CONNECTIONS_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| option_env!("MEETINGLY_CONNECTIONS_URL").map(str::to_string))
        .unwrap_or_else(|| CONNECTIONS_DEFAULT_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

async fn finish_authorization(url: Url) -> Result<AuthSession> {
    ensure_organization_switching_allowed().map_err(anyhow::Error::msg)?;
    if url.scheme() != "meetingly" || url.host_str() != Some("oauth") || url.path() != "/callback" {
        return Err(anyhow!("Unexpected OAuth callback URL."));
    }
    let params = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(error) = params.get("error") {
        return Err(anyhow!("Clerk rejected sign-in: {error}"));
    }
    let code = params
        .get("code")
        .context("OAuth callback did not include a code.")?;
    let state = params
        .get("state")
        .context("OAuth callback did not include state.")?;
    let pending = PENDING
        .write()
        .map_err(|_| anyhow!("OAuth state lock is unavailable"))?
        .take()
        .context("No sign-in is pending.")?;
    if state.as_ref() != pending.state {
        return Err(anyhow!("OAuth state did not match."));
    }
    let metadata = metadata().await?;
    let client_id = client_id()?;
    let response = reqwest::Client::new()
        .post(metadata.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_ref()),
            ("client_id", client_id.as_str()),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", pending.verifier.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    let session = validate_access_token(&response.access_token).await?;
    let tokens = StoredTokens {
        access_token: response.access_token,
        refresh_token: response
            .refresh_token
            .context("Clerk did not return a refresh token.")?,
        expires_at: response
            .expires_in
            .map(|seconds| chrono::Utc::now().timestamp() + seconds)
            .unwrap_or(session.expires_at),
    };
    save_tokens(&tokens)?;
    set_current_session(Some(session.clone()));
    Ok(session)
}

async fn refresh_tokens(current: StoredTokens) -> Result<StoredTokens> {
    let client_id = client_id()?;
    let response = reqwest::Client::new()
        .post(metadata().await?.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
            ("client_id", client_id.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    let session = validate_access_token(&response.access_token).await?;
    Ok(StoredTokens {
        access_token: response.access_token,
        refresh_token: response.refresh_token.unwrap_or(current.refresh_token),
        expires_at: response
            .expires_in
            .map(|seconds| chrono::Utc::now().timestamp() + seconds)
            .unwrap_or(session.expires_at),
    })
}

async fn metadata() -> Result<OAuthMetadata> {
    let url = format!("{}/.well-known/oauth-authorization-server", issuer()?);
    Ok(reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?)
}

async fn validate_access_token(token: &str) -> Result<AuthSession> {
    let payload = token
        .split('.')
        .nth(1)
        .context("Clerk access token is not a JWT.")?;
    let claims: AccessClaims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload)?)?;
    let client_id = client_id()?;
    if claims.iss.trim_end_matches('/') != issuer()?
        || claims
            .aud
            .as_ref()
            .is_some_and(|audience| !audience_contains(audience, &client_id))
    {
        return Err(anyhow!("Clerk access token issuer or audience is invalid."));
    }
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err(anyhow!("Clerk access token has expired."));
    }
    let verified = reqwest::Client::new()
        .get(format!("{}/api/auth/session", connections_url()))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<VerifiedSession>()
        .await?;
    let verified_org = verified
        .session_claims
        .get("org_id")
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            verified
                .session_claims
                .get("o")
                .and_then(|organization| organization.get("id"))
                .and_then(serde_json::Value::as_str)
        });
    if verified.user_id != claims.sub || verified_org != Some(claims.org_id.as_str()) {
        return Err(anyhow!(
            "Connections rejected the Clerk user or organization."
        ));
    }
    Ok(AuthSession {
        user_id: claims.sub,
        clerk_org_id: claims.org_id,
        expires_at: claims.exp,
    })
}

fn audience_contains(audience: &serde_json::Value, client_id: &str) -> bool {
    audience.as_str() == Some(client_id)
        || audience
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value.as_str() == Some(client_id)))
}

async fn open_portal(path: &str) -> std::result::Result<(), String> {
    crate::api::api::open_external_url(format!("{}/{path}", issuer().map_err(display_error)?)).await
}

fn issuer() -> Result<String> {
    config_value(
        "MEETINGLY_CLERK_ISSUER",
        option_env!("MEETINGLY_CLERK_ISSUER"),
    )
}

fn client_id() -> Result<String> {
    config_value(
        "MEETINGLY_CLERK_OAUTH_CLIENT_ID",
        option_env!("MEETINGLY_CLERK_OAUTH_CLIENT_ID"),
    )
}

fn config_value(name: &str, built: Option<&str>) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            built
                .map(str::to_string)
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| value.trim_end_matches('/').to_string())
        .ok_or_else(|| anyhow!("{name} is not configured."))
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn load_tokens() -> Result<StoredTokens> {
    let value = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?.get_password()?;
    Ok(serde_json::from_str(&value)?)
}

fn save_tokens(tokens: &StoredTokens) -> Result<()> {
    Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?
        .set_password(&serde_json::to_string(tokens)?)?;
    Ok(())
}

fn delete_tokens() -> Result<()> {
    let entry = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?;
    if entry.get_password().is_ok() {
        entry.delete_credential()?;
    }
    Ok(())
}

fn current_session() -> Option<AuthSession> {
    CURRENT_SESSION
        .read()
        .ok()
        .and_then(|session| session.clone())
}

fn set_current_session(session: Option<AuthSession>) {
    if let Ok(mut current) = CURRENT_SESSION.write() {
        *current = session;
    }
}

fn recording_identity() -> Option<OperationIdentity> {
    RECORDING_IDENTITY
        .read()
        .ok()
        .and_then(|identity| identity.clone())
}

fn current_operation_identity() -> Option<OperationIdentity> {
    current_session().map(|session| OperationIdentity::new(session.clerk_org_id, session.user_id))
}

fn clear_recording_identity() {
    if let Ok(mut identity) = RECORDING_IDENTITY.write() {
        *identity = None;
    }
}

#[cfg(test)]
pub(crate) fn set_test_session(identity: Option<OperationIdentity>) {
    set_current_session(identity.map(|identity| AuthSession {
        user_id: identity.user_id,
        clerk_org_id: identity.clerk_org_id,
        expires_at: i64::MAX,
    }));
}

#[cfg(test)]
pub(crate) fn retained_test_identity() -> Option<OperationIdentity> {
    recording_identity()
}

#[cfg(test)]
pub(crate) fn reset_test_auth() {
    clear_recording_identity();
    set_current_session(None);
}

pub(crate) fn finish_recording_identity_if_matches(identity: &OperationIdentity) -> bool {
    let Ok(mut retained) = RECORDING_IDENTITY.write() else {
        return false;
    };
    if retained.as_ref() != Some(identity) {
        return false;
    }
    *retained = None;
    true
}

fn ensure_organization_switching_allowed() -> std::result::Result<(), String> {
    if recording_identity().is_some() {
        Err("Stop and finish saving the active recording before switching organizations or signing out.".to_string())
    } else {
        Ok(())
    }
}

fn display_error(error: impl std::fmt::Display) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_callback_and_audience_shapes() {
        let _state = AUTH_TEST_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        assert!(audience_contains(&serde_json::json!("client"), "client"));
        assert!(audience_contains(
            &serde_json::json!(["other", "client"]),
            "client"
        ));
        assert!(!audience_contains(&serde_json::json!(["other"]), "client"));
        let url = Url::parse("meetingly://oauth/callback?code=x&state=y").unwrap();
        assert_eq!(url.host_str(), Some("oauth"));
        assert_eq!(url.path(), "/callback");
    }

    #[test]
    fn parses_clerk_oauth_access_claims_without_audience() {
        let claims: AccessClaims = serde_json::from_value(serde_json::json!({
            "sub": "user-a",
            "org_id": "org-a",
            "azp": "meetings-client",
            "exp": i64::MAX,
            "iss": "https://clerk.example"
        }))
        .unwrap();

        assert_eq!(claims.sub, "user-a");
        assert_eq!(claims.org_id, "org-a");
    }

    #[test]
    fn recording_identity_stays_bound_to_starting_organization() {
        let _state = AUTH_TEST_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        clear_recording_identity();
        set_current_session(Some(AuthSession {
            user_id: "user-a".to_string(),
            clerk_org_id: "org-a".to_string(),
            expires_at: i64::MAX,
        }));
        let guard = begin_recording_identity().unwrap();
        guard.commit();
        set_current_session(Some(AuthSession {
            user_id: "user-b".to_string(),
            clerk_org_id: "org-b".to_string(),
            expires_at: i64::MAX,
        }));
        assert_eq!(require_clerk_org_id().unwrap(), "org-a");
        assert_eq!(require_user_id().unwrap(), "user-a");
        assert_eq!(require_current_clerk_org_id().unwrap(), "org-b");
        assert!(auth_finish_recording_scope().is_err());
        assert!(finish_recording_identity_if_matches(&OperationIdentity::new(
            "org-a", "user-a"
        )));
        assert_eq!(require_clerk_org_id().unwrap(), "org-b");
        set_current_session(None);
    }

    #[test]
    fn recovery_releases_only_the_matching_recording_identity() {
        let _state = AUTH_TEST_MUTEX.lock().unwrap_or_else(|error| error.into_inner());
        clear_recording_identity();
        set_current_session(Some(AuthSession {
            user_id: "user-a".to_string(),
            clerk_org_id: "org-a".to_string(),
            expires_at: i64::MAX,
        }));
        begin_recording_identity().unwrap().commit();

        assert!(!finish_recording_identity_if_matches(
            &OperationIdentity::new("org-a", "user-b")
        ));
        assert_eq!(
            recording_identity(),
            Some(OperationIdentity::new("org-a", "user-a"))
        );
        assert!(finish_recording_identity_if_matches(
            &OperationIdentity::new("org-a", "user-a")
        ));
        assert_eq!(recording_identity(), None);
        set_current_session(None);
    }
}
