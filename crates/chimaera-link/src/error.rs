//! Typed outcomes callers must distinguish from ordinary network failures.

/// The account definitively refused this device's refresh token (revoked,
/// expired or already used). The client has cleared its credentials and
/// published `None`; only a new sign-in restores access. Never retry.
#[derive(Debug)]
pub struct AuthorizationRevoked;
impl std::fmt::Display for AuthorizationRevoked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("device authorization revoked")
    }
}
impl std::error::Error for AuthorizationRevoked {}
