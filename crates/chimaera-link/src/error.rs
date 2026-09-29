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

/// The account service does not offer what this client version needs (an
/// older service without a required route, or a different protocol major).
/// Retrying cannot help until one side is updated; callers show a stable
/// message instead of "retrying".
#[derive(Debug)]
pub struct ServiceUnsupported;
impl std::fmt::Display for ServiceUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the account service does not support this client version")
    }
}
impl std::error::Error for ServiceUnsupported {}
