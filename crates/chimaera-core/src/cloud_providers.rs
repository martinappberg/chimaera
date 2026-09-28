//! Shared provider identity and browser-origin policy. Authentication adapters
//! live in the daemon; desktop and browser clients read this same catalog.
use serde::Deserialize;
use std::sync::OnceLock;

#[derive(Debug, Deserialize)]
pub struct CloudProviderDefinition {
    pub id: String,
    pub label: String,
    pub category: String,
    pub auth_origins: Vec<String>,
}

#[derive(Deserialize)]
struct Catalog {
    providers: Vec<CloudProviderDefinition>,
}

pub fn provider_definitions() -> &'static [CloudProviderDefinition] {
    static CATALOG: OnceLock<Catalog> = OnceLock::new();
    &CATALOG
        .get_or_init(|| {
            serde_json::from_str(include_str!("cloud-providers.json"))
                .expect("embedded provider catalog must be valid")
        })
        .providers
}

pub fn provider_definition(id: &str) -> Option<&'static CloudProviderDefinition> {
    provider_definitions()
        .iter()
        .find(|provider| provider.id == id)
}

pub fn provider_auth_origins(id: &str) -> &'static [String] {
    provider_definition(id).map_or(&[], |provider| provider.auth_origins.as_slice())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_unique_and_origins_are_explicit() {
        let providers = provider_definitions();
        for (index, provider) in providers.iter().enumerate() {
            assert!(!provider.id.is_empty());
            assert!(provider
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b == b'-'));
            assert!(!provider.label.is_empty());
            assert!(matches!(provider.category.as_str(), "agent" | "repository"));
            assert!(!providers[..index]
                .iter()
                .any(|other| other.id == provider.id));
            for origin in &provider.auth_origins {
                assert!(origin.starts_with("https://"));
                assert!(!origin[8..].contains(['/', ':', '?', '#', '@']));
            }
        }
        assert!(provider_definition("not-installed").is_none());
        assert!(provider_auth_origins("not-installed").is_empty());
    }
}
