//! Closed opt-in fixture observations; never prints CLI/header/body values.
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
/// Fields cannot be constructed by sibling modules; only validated fixture startup
/// mints a trace. Ordinary consumers carry None and emit nothing.
pub(super) struct Trace {
    emitted: AtomicU64,
}
impl Trace {
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    pub(super) fn capture(
        state: &Arc<crate::AppState>,
    ) -> Result<Arc<Self>, chimaera_core::provider_runtime::Error> {
        use chimaera_core::provider_runtime::Error;
        let pending = crate::lock(&state.pro.execution.provider_pending)
            .clone()
            .ok_or(Error::Inactive)?;
        let uid = unsafe { nix::libc::geteuid() };
        if std::env::var("CHIMAERA_WORKER").as_deref() != Ok("1")
            || !(20000..30000).contains(&uid)
            || unsafe { nix::libc::getegid() } != uid
            || pending.launch.account_id != "controller-fixture"
            || pending.launch.workspace_id != "fixture-project-a"
        {
            return Err(Error::StateChanged);
        }
        pending
            .protection
            .current()
            .map_err(|_| Error::StateChanged)?;
        Ok(Arc::new(Self {
            emitted: AtomicU64::new(0),
        }))
    }
}
#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Stage {
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    Verified,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    OuterAdmitted,
    ConfigRefused,
    FrontendStartRefused,
    ChildSpawnRefused,
    ErrorInvalidRequest,
    ErrorInactive,
    ErrorUnsupported,
    ErrorStateChanged,
    ErrorNeedsSignIn,
    ErrorLimitReached,
    ErrorUnavailable,
    ConfigCaptured,
    FrontendStarted,
    ChildSpawned,
    StdinEof,
    ChildSuccess,
    ChildNonzero,
    ChildSignaled,
    ChildWaitRefused,
    ChildCleaned,
    FrontendCleaned,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    OutputMarker,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    OutputNoMarker,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    ReceiptWritten,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    ReceiptRefused,
    #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
    PublicRefused,
    FrontendAccepted,
    FrontendHeaderRefused,
    FrontendRequestRefused,
    FrontendPinned,
    FrontendOwnerRefused,
    FrontendResponse,
    FrontendRelayRefused,
}
impl Stage {
    fn label(self) -> &'static str {
        match self {
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::Verified => "verified",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::OuterAdmitted => "outer-admitted",
            Self::ConfigRefused => "config-refused",
            Self::FrontendStartRefused => "frontend-start-refused",
            Self::ChildSpawnRefused => "child-spawn-refused",
            Self::ErrorInvalidRequest => "error-invalid-request",
            Self::ErrorInactive => "error-inactive",
            Self::ErrorUnsupported => "error-unsupported",
            Self::ErrorStateChanged => "error-state-changed",
            Self::ErrorNeedsSignIn => "error-needs-sign-in",
            Self::ErrorLimitReached => "error-limit-reached",
            Self::ErrorUnavailable => "error-unavailable",
            Self::ConfigCaptured => "config-captured",
            Self::FrontendStarted => "frontend-started",
            Self::ChildSpawned => "child-spawned",
            Self::StdinEof => "stdin-eof",
            Self::ChildSuccess => "child-success",
            Self::ChildNonzero => "child-nonzero",
            Self::ChildSignaled => "child-signaled",
            Self::ChildWaitRefused => "child-wait-refused",
            Self::ChildCleaned => "child-cleaned",
            Self::FrontendCleaned => "frontend-cleaned",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::OutputMarker => "output-marker",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::OutputNoMarker => "output-no-marker",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::ReceiptWritten => "receipt-written",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::ReceiptRefused => "receipt-refused",
            #[cfg(all(target_os = "linux", feature = "provider-claude-fixture"))]
            Self::PublicRefused => "public-refused",
            Self::FrontendAccepted => "frontend-accepted",
            Self::FrontendHeaderRefused => "frontend-header-refused",
            Self::FrontendRequestRefused => "frontend-request-refused",
            Self::FrontendPinned => "frontend-pinned",
            Self::FrontendOwnerRefused => "frontend-owner-refused",
            Self::FrontendResponse => "frontend-response",
            Self::FrontendRelayRefused => "frontend-relay-refused",
        }
    }
}
pub(super) fn emit(trace: &Option<Arc<Trace>>, stage: Stage) {
    if let Some(trace) = trace {
        let bit = 1u64 << stage as u8;
        // At most one line per closed stage for the entire fixture lifetime.
        if trace.emitted.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            eprintln!("provider claude diagnostic: {}", stage.label());
        }
    }
}

pub(super) fn error(trace: &Option<Arc<Trace>>, error: chimaera_core::provider_runtime::Error) {
    use chimaera_core::provider_runtime::Error;
    emit(
        trace,
        match error {
            Error::InvalidRequest => Stage::ErrorInvalidRequest,
            Error::Inactive => Stage::ErrorInactive,
            Error::Unsupported => Stage::ErrorUnsupported,
            Error::StateChanged => Stage::ErrorStateChanged,
            Error::NeedsSignIn => Stage::ErrorNeedsSignIn,
            Error::LimitReached => Stage::ErrorLimitReached,
            Error::Unavailable => Stage::ErrorUnavailable,
        },
    );
}
