use anyhow::{bail, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};
use url::Url;

/// Secrets are deliberately not Debug. The caller stores token pairs in its OS
/// keychain; this crate only holds them in memory.
pub struct Pkce {
    pub verifier: String,
    pub challenge: String,
    pub state: String,
}
impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}
impl Pkce {
    pub fn new() -> Self {
        let verifier = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
        Self {
            challenge: Self::challenge_for(&verifier),
            verifier,
            state: URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>()),
        }
    }
    pub fn challenge_for(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }
    pub fn authorization_url(&self, endpoint: &str, redirect_uri: &str) -> Result<Url> {
        validate_redirect(redirect_uri)?;
        let mut url = crate::transport::path(
            &crate::transport::endpoint(endpoint)?,
            &["v1", "oauth", "authorize"],
        );
        url.query_pairs_mut().extend_pairs([
            ("response_type", "code"),
            ("client_id", "chimaera"),
            ("redirect_uri", redirect_uri),
            ("state", &self.state),
            ("code_challenge", &self.challenge),
            ("code_challenge_method", "S256"),
        ]);
        Ok(url)
    }
    pub fn callback_code(&self, callback: &Url) -> Result<String> {
        validate_redirect(callback.as_str().split('?').next().unwrap_or_default())?;
        let pairs: Vec<_> = callback.query_pairs().collect();
        let states: Vec<_> = pairs.iter().filter(|(key, _)| key == "state").collect();
        if states.len() != 1 || states[0].1 != self.state {
            bail!("OAuth state mismatch");
        }
        if pairs.iter().any(|(key, _)| key == "error") {
            bail!("authorization was declined");
        }
        let codes: Vec<_> = pairs.iter().filter(|(key, _)| key == "code").collect();
        if codes.len() != 1 || codes[0].1.is_empty() {
            bail!("missing or duplicate authorization code");
        }
        Ok(codes[0].1.to_string())
    }
}
pub fn validate_redirect(value: &str) -> Result<()> {
    let url = Url::parse(value)?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || url.port().is_none()
        || url.path() != "/callback"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!("expected a loopback OAuth callback");
    }
    Ok(())
}
