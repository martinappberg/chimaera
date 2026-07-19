use std::sync::OnceLock;

/// One connection pool for the shell's authenticated loopback API traffic.
/// Every request still supplies its own bearer token and timeout; sharing the
/// agent only reuses transports instead of rebuilding a TLS/HTTP stack for
/// each health probe or dashboard refresh.
pub(crate) fn agent() -> &'static ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT.get_or_init(|| ureq::Agent::config_builder().build().into())
}
