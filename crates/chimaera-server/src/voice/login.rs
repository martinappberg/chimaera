//! The claude.ai login dictation rides on — the one `claude` keeps on this
//! host. Read-only: chimaera never refreshes it (claude's refresh tokens
//! rotate, so a second refresher would sign claude out); an expired login
//! says so and claude renews it on its next request.

/// Why there is no usable login.
#[derive(Debug)]
pub(crate) enum LoginError {
    /// No claude.ai login on this host (not signed in, or an API key).
    NoLogin,
    /// Signed in, but the access token has expired.
    Expired,
    /// The login exists but could not be read.
    Unreadable(String),
}

impl LoginError {
    /// The wire `code` for the UI.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            LoginError::NoLogin => "no_login",
            LoginError::Expired => "expired",
            LoginError::Unreadable(_) => "unreadable",
        }
    }
}

impl std::fmt::Display for LoginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoginError::NoLogin => write!(
                f,
                "Voice dictation uses Claude's speech service and needs a Claude.ai login on this host. Sign in with /login in a Claude chat."
            ),
            LoginError::Expired => write!(
                f,
                "Claude's login on this host has expired. Send a message in any Claude chat (Claude renews it), then try again."
            ),
            LoginError::Unreadable(why) => write!(f, "Could not read Claude's login on this host: {why}"),
        }
    }
}

/// The current claude.ai access token.
pub(crate) async fn access_token() -> Result<String, LoginError> {
    Err(LoginError::NoLogin)
}
