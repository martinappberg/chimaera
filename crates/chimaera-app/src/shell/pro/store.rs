//! Credential keys must not let isolated previews rotate each other's sessions.
use anyhow::{Context, Result};
use std::path::Path;

pub(super) fn session_key(
    endpoint: &str,
    development: bool,
    isolated_config: Option<&Path>,
) -> Result<(&'static str, String)> {
    match isolated_config {
        Some(config) => {
            let config = config
                .to_str()
                .context("invalid account credential scope")?;
            let service = if development {
                "chimaera.dev.pro.isolated.v1"
            } else {
                "chimaera.pro.isolated.v1"
            };
            // An unambiguous tuple avoids delimiter collisions. Legacy entries
            // contain no installation binding and must never be guessed/imported.
            Ok((service, serde_json::to_string(&(endpoint, config))?))
        }
        None => Ok((
            if development {
                "chimaera.dev.pro"
            } else {
                "chimaera.pro"
            },
            endpoint.to_owned(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normal_app_keeps_its_existing_credential_key() {
        assert_eq!(
            session_key("https://account.example", false, None).unwrap(),
            ("chimaera.pro", "https://account.example".into())
        );
    }
    #[test]
    fn previews_are_bound_to_config_and_never_reuse_legacy_entries() {
        let endpoint = "https://account.example";
        let one = session_key(endpoint, true, Some(Path::new("/preview/one/config"))).unwrap();
        let two = session_key(endpoint, true, Some(Path::new("/preview/two/config"))).unwrap();
        let other_service = session_key(
            "https://staging.example",
            true,
            Some(Path::new("/preview/one/config")),
        )
        .unwrap();
        let release = session_key(endpoint, false, Some(Path::new("/preview/one/config"))).unwrap();
        assert_ne!(one, two);
        assert_ne!(one, other_service);
        assert_ne!(one, release);
        assert_ne!(one, session_key(endpoint, true, None).unwrap());
        assert_eq!(
            one,
            session_key(endpoint, true, Some(Path::new("/preview/one/config"))).unwrap()
        );
    }
}
