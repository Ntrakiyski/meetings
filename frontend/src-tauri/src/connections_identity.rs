use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use url::Url;

pub const CONTRACT: &str = "meetings-v1";
pub const CLIENT_ID: &str = "meetings-desktop-v1";
pub const REDIRECT_URI: &str = "meetingly://oauth/callback";
pub const SCOPES: &str = "meetings:read meetings:write offline_access";

/// Captured from Connections' verified identity response, never from token claims or a browser selector.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationIdentity {
    pub issuer: String,
    pub organization_id: String,
    pub workspace_id: String,
    pub user_id: String,
}

impl OperationIdentity {
    pub fn new(
        issuer: impl Into<String>,
        organization_id: impl Into<String>,
        workspace_id: impl Into<String>,
        user_id: impl Into<String>,
    ) -> Self {
        Self {
            issuer: issuer.into(),
            organization_id: organization_id.into(),
            workspace_id: workspace_id.into(),
            user_id: user_id.into(),
        }
    }

    /// Matches SQLite's generated ownership_key; legacy rows have no verified key.
    pub fn scope_key(&self) -> String {
        serde_json::to_string(&(&self.issuer, &self.organization_id, &self.workspace_id))
            .expect("serializing identity strings cannot fail")
    }
}

#[derive(Debug, Deserialize)]
pub struct OAuthMetadata {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub revocation_endpoint: Option<String>,
    pub code_challenge_methods_supported: Vec<String>,
}

impl OAuthMetadata {
    pub fn validate(&self, origin: &str) -> Result<()> {
        if self.issuer != format!("{origin}/api/identity")
            || self.authorization_endpoint != format!("{}/oauth2/authorize", self.issuer)
            || self.token_endpoint != format!("{}/oauth2/token", self.issuer)
            || self
                .revocation_endpoint
                .as_deref()
                .is_some_and(|endpoint| endpoint != format!("{}/oauth2/revoke", self.issuer))
            || !self
                .code_challenge_methods_supported
                .iter()
                .any(|method| method == "S256")
        {
            return Err(anyhow!("Connections OAuth discovery does not match the configured issuer and PKCE contract."));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedIdentity {
    pub contract: String,
    pub user_id: String,
    pub organization_id: String,
    pub workspace_id: String,
    pub expires_at: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LegacyIdentityResponse {
    contract: String,
    issuer: String,
    organization_id: String,
    workspace_id: String,
    user_id: String,
    mapping: Option<LegacyMapping>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct LegacyMapping {
    source_instance: String,
    legacy_organization_id: String,
    legacy_user_id: String,
}

pub struct VerifiedLegacyOwnership {
    pub(crate) identity: OperationIdentity,
    pub(crate) legacy_organization_id: String,
    pub(crate) legacy_user_id: String,
}

impl LegacyIdentityResponse {
    /// Response must come from the original issuer under the current live access grant.
    pub fn verify(self, identity: &OperationIdentity) -> Result<Option<VerifiedLegacyOwnership>> {
        if self.contract != CONTRACT
            || self.issuer != identity.issuer
            || self.organization_id != identity.organization_id
            || self.workspace_id != identity.workspace_id
            || self.user_id != identity.user_id
        {
            return Err(anyhow!(
                "Legacy recovery proof belongs to another identity."
            ));
        }
        let Some(mapping) = self.mapping else {
            return Ok(None);
        };
        if [
            &mapping.source_instance,
            &mapping.legacy_organization_id,
            &mapping.legacy_user_id,
        ]
        .iter()
        .any(|id| id.is_empty())
        {
            return Err(anyhow!("Legacy recovery proof is incomplete."));
        }
        Ok(Some(VerifiedLegacyOwnership {
            identity: identity.clone(),
            legacy_organization_id: mapping.legacy_organization_id,
            legacy_user_id: mapping.legacy_user_id,
        }))
    }
}

impl VerifiedIdentity {
    pub fn identity(&self, issuer: &str, now: i64) -> Result<OperationIdentity> {
        if self.contract != CONTRACT
            || self.expires_at <= now
            || [&self.user_id, &self.organization_id, &self.workspace_id]
                .iter()
                .any(|id| id.is_empty())
        {
            return Err(anyhow!(
                "Connections returned an invalid or expired Meetings identity."
            ));
        }
        Ok(OperationIdentity::new(
            issuer,
            &self.organization_id,
            &self.workspace_id,
            &self.user_id,
        ))
    }
}

pub fn normalized_origin(value: &str) -> Result<String> {
    let origin = value.trim_end_matches('/');
    let url = Url::parse(origin)?;
    let local =
        cfg!(debug_assertions) && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if url.origin().ascii_serialization() != origin
        || (url.scheme() != "https" && !(local && url.scheme() == "http"))
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(anyhow!(
            "Connections must use an exact HTTPS origin (debug builds may use loopback HTTP)."
        ));
    }
    Ok(origin.to_string())
}

pub fn validate_callback(url: &Url, state: &str, issuer: &str) -> Result<()> {
    if url.scheme() != "meetingly"
        || url.host_str() != Some("oauth")
        || url.path() != "/callback"
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(anyhow!("Unexpected OAuth callback URL."));
    }
    let mut params = std::collections::HashMap::new();
    for (name, value) in url.query_pairs() {
        if params.insert(name, value).is_some() {
            return Err(anyhow!("OAuth callback parameters must be unique."));
        }
    }
    if params.get("state").map(|value| value.as_ref()) != Some(state) {
        return Err(anyhow!("OAuth state did not match."));
    }
    if params.contains_key("error") {
        if params
            .get("iss")
            .is_some_and(|value| value.as_ref() != issuer)
        {
            return Err(anyhow!("OAuth issuer did not match."));
        }
    } else if params.get("iss").map(|value| value.as_ref()) != Some(issuer)
        || !params.get("code").is_some_and(|value| !value.is_empty())
    {
        return Err(anyhow!("OAuth callback issuer or code is invalid."));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovery_keeps_all_credentials_on_configured_issuer_with_s256() {
        let origin = "https://connections.example";
        let valid = || OAuthMetadata {
            issuer: format!("{origin}/api/identity"),
            authorization_endpoint: format!("{origin}/api/identity/oauth2/authorize"),
            token_endpoint: format!("{origin}/api/identity/oauth2/token"),
            revocation_endpoint: Some(format!("{origin}/api/identity/oauth2/revoke")),
            code_challenge_methods_supported: vec!["S256".into()],
        };
        assert!(valid().validate(origin).is_ok());
        let mut metadata = valid();
        metadata.token_endpoint = "https://other.example/token".into();
        assert!(metadata.validate(origin).is_err());
        let mut metadata = valid();
        metadata.revocation_endpoint = Some("https://other.example/revoke".into());
        assert!(metadata.validate(origin).is_err());
        let mut metadata = valid();
        metadata.code_challenge_methods_supported = vec!["plain".into()];
        assert!(metadata.validate(origin).is_err());
        let mut metadata = valid();
        metadata.issuer = "https://other.example/api/identity".into();
        assert!(metadata.validate(origin).is_err());
        for invalid in [
            "ftp://localhost",
            "http://connections.example",
            "https://connections.example/path",
            "https://user:password@connections.example",
            "https://connections.example?redirect=evil",
        ] {
            assert!(normalized_origin(invalid).is_err());
        }
    }
    #[test]
    fn callback_binds_state_issuer_and_exact_redirect() {
        let issuer = "https://connections.example/api/identity";
        let callback = Url::parse("meetingly://oauth/callback?code=code&state=state&iss=https%3A%2F%2Fconnections.example%2Fapi%2Fidentity").unwrap();
        assert!(validate_callback(&callback, "state", issuer).is_ok());
        assert!(validate_callback(&callback, "other-state", issuer).is_err());
        assert!(
            validate_callback(&callback, "state", "https://other.example/api/identity").is_err()
        );
        for raw in [
            "meetingly://oauth/callback?code=code&state=state&state=state",
            "meetingly://other/callback?code=code&state=state",
            "meetingly://oauth/callback/extra?code=code&state=state",
            "meetingly://oauth/callback?code=code&state=state",
            "meetingly://oauth/callback?error=access_denied&state=other",
        ] {
            assert!(validate_callback(&Url::parse(raw).unwrap(), "state", issuer).is_err());
        }
    }
    #[test]
    fn issuer_or_sibling_workspace_never_shares_an_ownership_key() {
        let a1 =
            OperationIdentity::new("https://a.example/api/identity", "company-a", "A1", "user");
        let a2 = OperationIdentity::new(&a1.issuer, &a1.organization_id, "A2", "user");
        let other =
            OperationIdentity::new("https://b.example/api/identity", "company-a", "A1", "user");
        assert_ne!(a1.scope_key(), a2.scope_key());
        assert_ne!(a1.scope_key(), other.scope_key());
    }
    #[test]
    fn verified_identity_requires_supported_contract_and_live_expiry() {
        let identity = VerifiedIdentity {
            contract: CONTRACT.into(),
            user_id: "user".into(),
            organization_id: "company".into(),
            workspace_id: "A1".into(),
            expires_at: 100,
        };
        assert!(identity
            .identity("https://connections.example/api/identity", 99)
            .is_ok());
        assert!(identity
            .identity("https://connections.example/api/identity", 100)
            .is_err());
        assert!(VerifiedIdentity {
            contract: "other".into(),
            ..identity
        }
        .identity("https://connections.example/api/identity", 99)
        .is_err());
    }
}
