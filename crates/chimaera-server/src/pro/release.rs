//! Shared typed refusal identity for the private clean-release policy.
//! Keeping this marker public preserves durable latch/error classification.
/// The account refused a legacy (v1) release because the project is enrolled
/// in managed execution: it must be released through v2. Typed so the caller
/// can latch the project for the newer path (`execution::require_v2`); the
/// conflict parse below would reject this body, which has no `baton`.
#[derive(Debug)]
pub struct UpgradeRequired;
impl std::fmt::Display for UpgradeRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("The account needs this project's newer transfer path; trying again shortly")
    }
}
impl std::error::Error for UpgradeRequired {}
