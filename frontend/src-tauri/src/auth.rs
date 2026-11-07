use anyhow::{anyhow, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use keyring::Entry;
use once_cell::sync::Lazy;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::RwLock;
use tauri::{App, AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_deep_link::DeepLinkExt;
use url::Url;

pub use crate::connections_identity::OperationIdentity;
use crate::connections_identity::{
    normalized_origin, validate_callback, OAuthMetadata, VerifiedIdentity, CLIENT_ID, CONTRACT,
    REDIRECT_URI, SCOPES,
};
const KEYCHAIN_SERVICE: &str = "meetingly-connections-oauth-v1";
const KEYCHAIN_ACCOUNT: &str = "session";
const CONNECTIONS_DEFAULT_URL: &str = "https://connections.fractals-solutions.com";

static CURRENT_SESSION: Lazy<RwLock<Option<AuthSession>>> = Lazy::new(|| RwLock::new(None));
static TOKEN_LOCK: Lazy<tokio::sync::Mutex<()>> = Lazy::new(|| tokio::sync::Mutex::new(()));
static HTTP_CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .expect("valid Connections identity HTTP client")
});
static PENDING: Lazy<RwLock<Option<PendingAuthorization>>> = Lazy::new(|| RwLock::new(None));
static RECORDING_IDENTITY: Lazy<RwLock<Option<OperationIdentity>>> =
    Lazy::new(|| RwLock::new(None));
#[cfg(test)]
pub(crate) static AUTH_TEST_MUTEX: Lazy<std::sync::Mutex<()>> =
    Lazy::new(|| std::sync::Mutex::new(()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AuthenticationSurface {
    SystemSession,
    ExternalBrowser,
}

fn authentication_surface_for(target_os: &str) -> AuthenticationSurface {
    if target_os == "macos" {
        AuthenticationSurface::SystemSession
    } else {
        AuthenticationSurface::ExternalBrowser
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
    pub issuer: String,
    pub organization_id: String,
    pub workspace_id: String,
    pub expires_at: i64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredTokens {
    identity: OperationIdentity,
    access_token: String,
    refresh_token: String,
    expires_at: i64,
}

#[derive(Clone, Debug)]
struct PendingAuthorization {
    state: String,
    verifier: String,
    issuer: String,
    resource: String,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    connections_contract: String,
    token_type: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
}

pub fn setup_deep_links<R: Runtime>(app: &App<R>) -> Result<()> {
    let handle = app.handle().clone();
    app.deep_link().on_open_url(move |event| {
        for url in event.urls() {
            let callback = url.clone();
            let app = handle.clone();
            tauri::async_runtime::spawn(async move {
                if let Err(error) = finish_authorization_and_emit(&app, callback).await {
                    log::error!("Connections OAuth callback failed: {error}");
                }
            });
        }
    });

    #[cfg(any(target_os = "linux", all(debug_assertions, windows)))]
    app.deep_link().register_all()?;
    Ok(())
}

#[tauri::command]
pub async fn auth_start_sign_in(app: AppHandle) -> std::result::Result<(), String> {
    ensure_workspace_switching_allowed()?;
    let authorization_url = prepare_authorization_url().await.map_err(display_error)?;
    launch_authorization(&app, authorization_url).await
}

async fn prepare_authorization_url() -> Result<Url> {
    let _tokens = TOKEN_LOCK.lock().await;
    let metadata = metadata().await?;
    let verifier = random_urlsafe(32);
    let challenge = URL_SAFE_NO_PAD.encode(ring::digest::digest(
        &ring::digest::SHA256,
        verifier.as_bytes(),
    ));
    let state = random_urlsafe(24);
    let mut authorization_url = Url::parse(&metadata.authorization_endpoint)?;
    authorization_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", CLIENT_ID)
        .append_pair("redirect_uri", REDIRECT_URI)
        .append_pair("scope", SCOPES)
        .append_pair("state", &state)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256")
        .append_pair("prompt", "consent")
        .append_pair(
            "resource",
            &format!("{}/api/meetings", normalized_origin(&connections_url())?),
        );
    let _recording = idle_recording_scope()?;
    *PENDING
        .write()
        .map_err(|_| anyhow!("OAuth state lock is unavailable"))? = Some(PendingAuthorization {
        state,
        verifier,
        issuer: metadata.issuer,
        resource: format!("{}/api/meetings", normalized_origin(&connections_url())?),
    });
    Ok(authorization_url)
}

#[tauri::command]
pub async fn auth_get_session() -> std::result::Result<Option<AuthSession>, String> {
    match access_token().await {
        Ok(_) => Ok(current_session()),
        Err(error) => {
            log::warn!("No usable Connections session: {error:#}");
            Ok(None)
        }
    }
}

#[tauri::command]
pub async fn auth_sign_out() -> std::result::Result<(), String> {
    let _tokens = TOKEN_LOCK.lock().await;
    // Clear local authority before awaiting revocation, while recording startup is excluded.
    let previous = {
        let _recording = idle_recording_scope().map_err(display_error)?;
        let previous = load_tokens().ok();
        delete_tokens().map_err(display_error)?;
        clear_pending_authorization().map_err(display_error)?;
        set_current_session(None);
        previous
    };
    if let Some(tokens) = previous {
        if let Ok(metadata) = metadata().await {
            if let Some(endpoint) = metadata.revocation_endpoint {
                let _ = HTTP_CLIENT
                    .post(endpoint)
                    .form(&[
                        ("token", tokens.refresh_token),
                        ("client_id", CLIENT_ID.to_string()),
                    ])
                    .send()
                    .await;
            }
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn auth_open_profile(app: AppHandle) -> std::result::Result<(), String> {
    let _ = app;
    crate::api::api::open_external_url(format!(
        "{}/account/security",
        normalized_origin(&connections_url()).map_err(display_error)?
    ))
    .await
}

#[tauri::command]
pub async fn auth_open_organization(app: AppHandle) -> std::result::Result<(), String> {
    ensure_workspace_switching_allowed()?;
    let _ = app;
    let identity = require_current_operation_identity().map_err(display_error)?;
    let mut url = Url::parse(&normalized_origin(&connections_url()).map_err(display_error)?)
        .map_err(display_error)?;
    url.path_segments_mut()
        .map_err(|_| "Connections origin cannot have path segments.".to_string())?
        .push("companies")
        .push(&identity.organization_id);
    crate::api::api::open_external_url(url.to_string()).await
}

#[tauri::command]
pub fn auth_finish_recording_scope() -> std::result::Result<(), String> {
    let identity = require_current_operation_identity().map_err(display_error)?;
    if finish_recording_identity_if_matches(&identity) {
        Ok(())
    } else {
        Err(
            "The retained recording belongs to a different Connections user or organization."
                .to_string(),
        )
    }
}

pub async fn access_token() -> Result<String> {
    let _tokens = TOKEN_LOCK.lock().await;
    let mut tokens = load_tokens()?;
    if tokens.expires_at <= chrono::Utc::now().timestamp() + 60 {
        tokens = refresh_tokens(tokens).await?;
        save_tokens(&tokens)?;
    }
    let session = validate_access_token(&tokens.access_token).await?;
    if session.operation_identity() != tokens.identity {
        return Err(anyhow!(
            "The stored token belongs to another Connections workspace or user."
        ));
    }
    set_current_session(Some(session));
    Ok(tokens.access_token)
}

pub fn require_scope_key() -> Result<String> {
    Ok(require_operation_identity()?.scope_key())
}

pub fn require_current_scope_key() -> Result<String> {
    Ok(require_current_operation_identity()?.scope_key())
}

pub fn require_user_id() -> Result<String> {
    if let Some(identity) = recording_identity() {
        return Ok(identity.user_id);
    }
    require_current_user_id()
}

pub fn require_current_user_id() -> Result<String> {
    Ok(require_current_operation_identity()?.user_id)
}

pub fn begin_recording_identity() -> Result<RecordingIdentityGuard> {
    begin_operation_identity()
}

pub fn begin_operation_identity() -> Result<RecordingIdentityGuard> {
    // Lock order matches session commits: recording scope, then current session.
    let mut locked = RECORDING_IDENTITY
        .write()
        .map_err(|_| anyhow!("Recording identity lock is unavailable"))?;
    if locked.is_some() {
        return Err(anyhow!("A recording workspace is already locked."));
    }
    let session =
        current_session().context("A verified Connections session is required to record.")?;
    if session.expires_at <= chrono::Utc::now().timestamp() || issuer()? != session.issuer {
        return Err(anyhow!(
            "Sign in to Connections before starting a new recording."
        ));
    }
    let identity = session.operation_identity();
    *locked = Some(identity.clone());
    Ok(RecordingIdentityGuard {
        identity,
        committed: false,
    })
}

pub fn require_operation_identity() -> Result<OperationIdentity> {
    recording_identity()
        .or_else(current_operation_identity)
        .context("A Connections workspace is required.")
}

pub fn require_current_operation_identity() -> Result<OperationIdentity> {
    current_operation_identity().context("A Connections workspace is required.")
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
    let _tokens = TOKEN_LOCK.lock().await;
    ensure_workspace_switching_allowed().map_err(anyhow::Error::msg)?;
    let expected = PENDING
        .read()
        .map_err(|_| anyhow!("OAuth state lock is unavailable"))?
        .clone()
        .context("No sign-in is pending.")?;
    validate_callback(&url, &expected.state, &expected.issuer)?;
    if issuer()? != expected.issuer {
        return Err(anyhow!(
            "Changing Connections issuer requires a new sign-in."
        ));
    }
    let pending = {
        let mut locked = PENDING
            .write()
            .map_err(|_| anyhow!("OAuth state lock is unavailable"))?;
        if locked.as_ref().map(|pending| pending.state.as_str()) != Some(expected.state.as_str()) {
            return Err(anyhow!("OAuth state changed during callback handling."));
        }
        locked.take().context("No sign-in is pending.")?
    };
    let params = url
        .query_pairs()
        .collect::<std::collections::HashMap<_, _>>();
    if let Some(error) = params.get("error") {
        return Err(anyhow!("Connections rejected sign-in: {error}"));
    }
    let code = params
        .get("code")
        .context("OAuth callback did not include a code.")?;
    let metadata = metadata().await?;
    if metadata.issuer != pending.issuer {
        return Err(anyhow!("Connections issuer changed during sign-in."));
    }
    let response = HTTP_CLIENT
        .post(metadata.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code.as_ref()),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("code_verifier", pending.verifier.as_str()),
            ("resource", pending.resource.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    let session = validate_access_token(&response.access_token).await?;
    let tokens = response.into_tokens(&session)?;
    commit_authorized_session(&tokens, &session)?;
    Ok(session)
}

fn commit_authorized_session(tokens: &StoredTokens, session: &AuthSession) -> Result<()> {
    // No await can let a recording begin between this check and the session commit.
    let _recording = idle_recording_scope()?;
    save_tokens(tokens)?;
    set_current_session(Some(session.clone()));
    Ok(())
}

fn idle_recording_scope() -> Result<std::sync::RwLockReadGuard<'static, Option<OperationIdentity>>>
{
    let recording = RECORDING_IDENTITY
        .read()
        .map_err(|_| anyhow!("Recording identity lock is unavailable"))?;
    if recording.is_some() {
        return Err(anyhow!("Stop and finish saving the active recording before switching workspaces or signing out."));
    }
    Ok(recording)
}

fn clear_pending_authorization() -> Result<()> {
    PENDING
        .write()
        .map_err(|_| anyhow!("OAuth state lock is unavailable"))?
        .take();
    Ok(())
}

async fn refresh_tokens(current: StoredTokens) -> Result<StoredTokens> {
    if current.identity.issuer != issuer()? {
        return Err(anyhow!(
            "Changing Connections issuer requires a new sign-in."
        ));
    }
    let resource = format!("{}/api/meetings", normalized_origin(&connections_url())?);
    let response = HTTP_CLIENT
        .post(metadata().await?.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", current.refresh_token.as_str()),
            ("client_id", CLIENT_ID),
            ("resource", resource.as_str()),
        ])
        .send()
        .await?
        .error_for_status()?
        .json::<TokenResponse>()
        .await?;
    let session = validate_access_token(&response.access_token).await?;
    if session.operation_identity() != current.identity {
        return Err(anyhow!(
            "Refresh cannot change the original workspace or user."
        ));
    }
    response.into_tokens(&session)
}

async fn metadata() -> Result<OAuthMetadata> {
    let origin = normalized_origin(&connections_url())?;
    let metadata: OAuthMetadata = HTTP_CLIENT
        .get(format!(
            "{origin}/.well-known/oauth-authorization-server/api/identity"
        ))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    metadata.validate(&origin)?;
    Ok(metadata)
}

async fn validate_access_token(token: &str) -> Result<AuthSession> {
    if !token.starts_with("cma_") {
        return Err(anyhow!("A Connections Meetings access token is required."));
    }
    let origin = normalized_origin(&connections_url())?;
    let verified = HTTP_CLIENT
        .get(format!("{origin}/api/meetings-identity"))
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json::<VerifiedIdentity>()
        .await?;
    let identity = verified.identity(
        &format!("{origin}/api/identity"),
        chrono::Utc::now().timestamp(),
    )?;
    Ok(AuthSession {
        issuer: identity.issuer,
        organization_id: identity.organization_id,
        workspace_id: identity.workspace_id,
        user_id: identity.user_id,
        expires_at: verified.expires_at,
    })
}

impl AuthSession {
    pub fn operation_identity(&self) -> OperationIdentity {
        OperationIdentity::new(
            &self.issuer,
            &self.organization_id,
            &self.workspace_id,
            &self.user_id,
        )
    }
}

impl TokenResponse {
    fn into_tokens(self, session: &AuthSession) -> Result<StoredTokens> {
        let seconds = self
            .expires_in
            .context("Connections did not return a token expiry.")?;
        let refresh_token = self
            .refresh_token
            .context("Connections did not return a refresh token.")?;
        if self.connections_contract != CONTRACT
            || !self.token_type.eq_ignore_ascii_case("bearer")
            || !self.access_token.starts_with("cma_")
            || !refresh_token.starts_with("cmr_")
            || seconds <= 0
            || seconds > 300
        {
            return Err(anyhow!(
                "Connections returned an unsupported Meetings token response."
            ));
        }
        Ok(StoredTokens {
            identity: session.operation_identity(),
            access_token: self.access_token,
            refresh_token,
            expires_at: session
                .expires_at
                .min(chrono::Utc::now().timestamp() + seconds),
        })
    }
}

async fn launch_authorization(
    app: &AppHandle,
    authorization_url: Url,
) -> std::result::Result<(), String> {
    match authentication_surface_for(std::env::consts::OS) {
        #[cfg(target_os = "macos")]
        AuthenticationSurface::SystemSession => {
            let window = app
                .get_webview_window("main")
                .ok_or_else(|| "Meetily's main window is unavailable.".to_string())?;
            let presentation_window = window.ns_window().map_err(display_error)? as usize;
            let callback = macos_web_auth::authenticate(authorization_url, presentation_window)
                .await
                .map_err(display_error)?;
            finish_authorization_and_emit(app, callback).await
        }
        #[cfg(not(target_os = "macos"))]
        AuthenticationSurface::SystemSession => {
            Err("The native authentication session is unavailable on this platform.".to_string())
        }
        AuthenticationSurface::ExternalBrowser => {
            let _ = app;
            crate::api::api::open_external_url(authorization_url.to_string()).await
        }
    }
}

async fn finish_authorization_and_emit<R: Runtime>(
    app: &AppHandle<R>,
    callback: Url,
) -> std::result::Result<(), String> {
    match finish_authorization(callback).await {
        Ok(session) => app
            .emit("auth-changed", Some(session))
            .map_err(display_error),
        Err(error) => {
            let message = error.to_string();
            let _ = app.emit("auth-error", message.clone());
            Err(message)
        }
    }
}

#[cfg(target_os = "macos")]
mod macos_web_auth {
    use anyhow::{anyhow, Context, Result};
    use futures_channel::oneshot;
    use once_cell::sync::Lazy;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::sync::Mutex;
    use url::Url;

    type Completion = oneshot::Sender<Result<Url>>;
    static COMPLETION: Lazy<Mutex<Option<Completion>>> = Lazy::new(|| Mutex::new(None));

    extern "C" {
        fn meetingly_start_web_auth_session(
            url: *const c_char,
            callback_scheme: *const c_char,
            presentation_window: *mut c_void,
            completion: extern "C" fn(*const c_char, *const c_char),
        ) -> i32;
    }

    pub async fn authenticate(url: Url, presentation_window: usize) -> Result<Url> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = COMPLETION
                .lock()
                .map_err(|_| anyhow!("The macOS authentication session lock is unavailable."))?;
            if pending.is_some() {
                return Err(anyhow!(
                    "A Connections authentication session is already open."
                ));
            }
            *pending = Some(sender);
        }

        let url = CString::new(url.as_str())?;
        let scheme = CString::new("meetingly")?;
        let started = unsafe {
            meetingly_start_web_auth_session(
                url.as_ptr(),
                scheme.as_ptr(),
                presentation_window as *mut c_void,
                complete,
            )
        };
        if started == 0 {
            if let Ok(mut pending) = COMPLETION.lock() {
                pending.take();
            }
            return Err(anyhow!("macOS rejected the web authentication request."));
        }

        receiver
            .await
            .context("The macOS authentication session ended without a callback.")?
    }

    extern "C" fn complete(callback_url: *const c_char, error_message: *const c_char) {
        let result = if !callback_url.is_null() {
            let callback = unsafe { CStr::from_ptr(callback_url) }.to_string_lossy();
            Url::parse(&callback).context("macOS returned an invalid OAuth callback URL.")
        } else if !error_message.is_null() {
            let message = unsafe { CStr::from_ptr(error_message) }.to_string_lossy();
            Err(anyhow!(message.into_owned()))
        } else {
            Err(anyhow!("The macOS authentication session was cancelled."))
        };

        if let Ok(mut pending) = COMPLETION.lock() {
            if let Some(sender) = pending.take() {
                let _ = sender.send(result);
            }
        }
    }
}

fn issuer() -> Result<String> {
    Ok(format!(
        "{}/api/identity",
        normalized_origin(&connections_url())?
    ))
}

fn random_urlsafe(bytes: usize) -> String {
    let mut value = vec![0_u8; bytes];
    rand::thread_rng().fill_bytes(&mut value);
    URL_SAFE_NO_PAD.encode(value)
}

fn load_tokens() -> Result<StoredTokens> {
    let value = Entry::new(KEYCHAIN_SERVICE, KEYCHAIN_ACCOUNT)?.get_password()?;
    let tokens: StoredTokens = serde_json::from_str(&value)?;
    if tokens.identity.issuer != issuer()? {
        return Err(anyhow!(
            "Stored tokens belong to another Connections issuer. Sign in again."
        ));
    }
    Ok(tokens)
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
    current_session()
        .filter(|session| issuer().ok().as_deref() == Some(session.issuer.as_str()))
        .map(|session| session.operation_identity())
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
        issuer: identity.issuer,
        organization_id: identity.organization_id,
        workspace_id: identity.workspace_id,
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

fn ensure_workspace_switching_allowed() -> std::result::Result<(), String> {
    if recording_identity().is_some() {
        Err("Stop and finish saving the active recording before switching workspaces or signing out.".to_string())
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
    fn authorization_cannot_commit_if_recording_started_during_exchange() {
        let _state = AUTH_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        reset_test_auth();
        let original = OperationIdentity::new(
            "https://connections.fractals-solutions.com/api/identity",
            "company-a",
            "A1",
            "user-a",
        );
        set_test_session(Some(original.clone()));
        begin_operation_identity().unwrap().commit();
        let replacement = AuthSession {
            user_id: "user-a".into(),
            issuer: original.issuer.clone(),
            organization_id: "company-b".into(),
            workspace_id: "B1".into(),
            expires_at: i64::MAX,
        };
        let tokens = StoredTokens {
            identity: replacement.operation_identity(),
            access_token: "unused-access".into(),
            refresh_token: "unused-refresh".into(),
            expires_at: i64::MAX,
        };
        assert!(commit_authorized_session(&tokens, &replacement).is_err());
        assert_eq!(current_operation_identity(), Some(original.clone()));
        assert_eq!(recording_identity(), Some(original));
        reset_test_auth();
    }

    #[tokio::test]
    async fn cancelled_pending_callback_cannot_restore_signed_out_session() {
        let _state = AUTH_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        reset_test_auth();
        *PENDING.write().unwrap() = Some(PendingAuthorization {
            state: "original-state".into(),
            verifier: "unused-verifier".into(),
            issuer: "https://connections.fractals-solutions.com/api/identity".into(),
            resource: "https://connections.fractals-solutions.com/api/meetings".into(),
        });
        clear_pending_authorization().unwrap();
        let callback = Url::parse("meetingly://oauth/callback?code=delayed-code&state=original-state&iss=https%3A%2F%2Fconnections.fractals-solutions.com%2Fapi%2Fidentity").unwrap();
        assert!(finish_authorization(callback)
            .await
            .unwrap_err()
            .to_string()
            .contains("No sign-in is pending"));
        assert!(current_session().is_none());
        reset_test_auth();
    }

    #[test]
    fn macos_authentication_owns_the_callback_session() {
        assert_eq!(
            authentication_surface_for("macos"),
            AuthenticationSurface::SystemSession
        );
        assert_eq!(
            authentication_surface_for("windows"),
            AuthenticationSurface::ExternalBrowser
        );
        assert_eq!(
            authentication_surface_for("linux"),
            AuthenticationSurface::ExternalBrowser
        );
    }

    #[test]
    fn recording_identity_stays_bound_to_starting_organization() {
        let _state = AUTH_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_recording_identity();
        set_current_session(Some(AuthSession {
            user_id: "user-a".to_string(),
            issuer: "https://connections.fractals-solutions.com/api/identity".to_string(),
            organization_id: "company-a".to_string(),
            workspace_id: "A1".to_string(),
            expires_at: i64::MAX,
        }));
        let guard = begin_recording_identity().unwrap();
        guard.commit();
        set_current_session(Some(AuthSession {
            user_id: "user-b".to_string(),
            issuer: "https://connections.fractals-solutions.com/api/identity".to_string(),
            organization_id: "company-b".to_string(),
            workspace_id: "B1".to_string(),
            expires_at: i64::MAX,
        }));
        assert_eq!(require_operation_identity().unwrap().workspace_id, "A1");
        assert_eq!(require_user_id().unwrap(), "user-a");
        assert_eq!(
            require_current_operation_identity().unwrap().workspace_id,
            "B1"
        );
        assert!(auth_finish_recording_scope().is_err());
        assert!(finish_recording_identity_if_matches(
            &OperationIdentity::new(
                "https://connections.fractals-solutions.com/api/identity",
                "company-a",
                "A1",
                "user-a"
            )
        ));
        assert_eq!(require_operation_identity().unwrap().workspace_id, "B1");
        set_current_session(None);
    }

    #[test]
    fn recovery_releases_only_the_matching_recording_identity() {
        let _state = AUTH_TEST_MUTEX
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        clear_recording_identity();
        set_current_session(Some(AuthSession {
            user_id: "user-a".to_string(),
            issuer: "https://connections.fractals-solutions.com/api/identity".to_string(),
            organization_id: "company-a".to_string(),
            workspace_id: "A1".to_string(),
            expires_at: i64::MAX,
        }));
        begin_recording_identity().unwrap().commit();

        assert!(!finish_recording_identity_if_matches(
            &OperationIdentity::new(
                "https://connections.fractals-solutions.com/api/identity",
                "company-a",
                "A1",
                "user-b"
            )
        ));
        assert_eq!(
            recording_identity(),
            Some(OperationIdentity::new(
                "https://connections.fractals-solutions.com/api/identity",
                "company-a",
                "A1",
                "user-a"
            ))
        );
        assert!(finish_recording_identity_if_matches(
            &OperationIdentity::new(
                "https://connections.fractals-solutions.com/api/identity",
                "company-a",
                "A1",
                "user-a"
            )
        ));
        assert_eq!(recording_identity(), None);
        set_current_session(None);
    }
}
