//! Closed opt-in fixture observations; never prints CLI/header/body values.
use std::sync::{
    atomic::{AtomicU64, AtomicU8, Ordering},
    Arc,
};
/// Fields cannot be constructed by sibling modules; only validated fixture startup
/// mints a trace. Ordinary consumers carry None and emit nothing.
pub(super) struct Trace {
    emitted: AtomicU64,
    raw_reason: AtomicU8,
    request_reason: AtomicU8,
    accepted: AtomicU8,
    raw_refused: AtomicU8,
    request_refused: AtomicU8,
}
impl Trace {
    #[cfg(any(test, all(target_os = "linux", feature = "provider-claude-fixture")))]
    fn new() -> Arc<Self> {
        Arc::new(Self {
            emitted: AtomicU64::new(0),
            raw_reason: AtomicU8::new(0),
            request_reason: AtomicU8::new(0),
            accepted: AtomicU8::new(0),
            raw_refused: AtomicU8::new(0),
            request_refused: AtomicU8::new(0),
        })
    }
    #[cfg(test)]
    pub(super) fn synthetic() -> Arc<Self> {
        Self::new()
    }
    #[cfg(test)]
    pub(super) fn counts(&self) -> (u8, u8, u8) {
        (
            self.accepted.load(Ordering::Acquire),
            self.raw_refused.load(Ordering::Acquire),
            self.request_refused.load(Ordering::Acquire),
        )
    }
    #[cfg(test)]
    pub(super) fn reasons(&self) -> (u8, u8) {
        (
            self.raw_reason.load(Ordering::Acquire),
            self.request_reason.load(Ordering::Acquire),
        )
    }
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
        Ok(Self::new())
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

/// First refusal at each independent gate; no raw header, target or token data.
/// Separate cells keep raw EOF/probes from concealing an authorized request's
/// metadata failure, without widening the existing stage bitmap.
#[derive(Clone, Copy)]
#[repr(u8)]
pub(super) enum Refusal {
    RawOwner,
    RawIo,
    RawEof,
    RawUtf8,
    RawLine,
    RawTransfer,
    RawOrigin,
    RawApiKey,
    RawCount,
    RawLengthMissing,
    RawLengthDuplicate,
    RawAuthMissing,
    RawAuthDuplicate,
    RawHostMissing,
    RawHostDuplicate,
    RawContentMissing,
    RawContentDuplicate,
    RawDeadline,
    RawCap,
    RequestOwner,
    RequestHeaderCount,
    RequestHeaderText,
    RequestMethod,
    RequestUri,
    RequestHost,
    RequestTransfer,
    RequestOrigin,
    RequestApiKey,
    RequestCount,
    RequestCap,
    RequestContent,
    RequestAuthShape,
    RequestAuthMismatch,
    RequestRoute,
    RequestLength,
    RequestBodyCount,
    RequestVersionHeader,
    RequestBetaHeader,
    RequestUserAgentHeader,
    RequestVersion,
    RequestUserAgent,
    RequestBeta,
}
impl Refusal {
    fn label(self) -> &'static str {
        match self {
            Self::RawOwner => "frontend-raw-owner",
            Self::RawIo => "frontend-raw-io",
            Self::RawEof => "frontend-raw-eof",
            Self::RawUtf8 => "frontend-raw-utf8",
            Self::RawLine => "frontend-raw-line",
            Self::RawTransfer => "frontend-raw-transfer",
            Self::RawOrigin => "frontend-raw-origin",
            Self::RawApiKey => "frontend-raw-api-key",
            Self::RawCount => "frontend-raw-count",
            Self::RawLengthMissing => "frontend-raw-length-missing",
            Self::RawLengthDuplicate => "frontend-raw-length-duplicate",
            Self::RawAuthMissing => "frontend-raw-auth-missing",
            Self::RawAuthDuplicate => "frontend-raw-auth-duplicate",
            Self::RawHostMissing => "frontend-raw-host-missing",
            Self::RawHostDuplicate => "frontend-raw-host-duplicate",
            Self::RawContentMissing => "frontend-raw-content-missing",
            Self::RawContentDuplicate => "frontend-raw-content-duplicate",
            Self::RawDeadline => "frontend-raw-deadline",
            Self::RawCap => "frontend-raw-cap",
            Self::RequestOwner => "frontend-request-owner",
            Self::RequestHeaderCount => "frontend-request-header-count",
            Self::RequestHeaderText => "frontend-request-header-text",
            Self::RequestMethod => "frontend-request-method",
            Self::RequestUri => "frontend-request-uri",
            Self::RequestHost => "frontend-request-host",
            Self::RequestTransfer => "frontend-request-transfer",
            Self::RequestOrigin => "frontend-request-origin",
            Self::RequestApiKey => "frontend-request-api-key",
            Self::RequestCount => "frontend-request-count",
            Self::RequestCap => "frontend-request-cap",
            Self::RequestContent => "frontend-request-content",
            Self::RequestAuthShape => "frontend-request-auth-shape",
            Self::RequestAuthMismatch => "frontend-request-auth-mismatch",
            Self::RequestRoute => "frontend-request-route",
            Self::RequestLength => "frontend-request-length",
            Self::RequestBodyCount => "frontend-request-body-count",
            Self::RequestVersionHeader => "frontend-request-version-header",
            Self::RequestBetaHeader => "frontend-request-beta-header",
            Self::RequestUserAgentHeader => "frontend-request-user-agent-header",
            Self::RequestVersion => "frontend-request-version",
            Self::RequestUserAgent => "frontend-request-user-agent",
            Self::RequestBeta => "frontend-request-beta",
        }
    }
}
pub(super) fn refusal(
    trace: &Option<Arc<Trace>>,
    reason: Refusal,
    error: chimaera_core::provider_runtime::Error,
) -> chimaera_core::provider_runtime::Error {
    if let Some(trace) = trace {
        let cell = if (reason as u8) <= Refusal::RawCap as u8 {
            &trace.raw_reason
        } else {
            &trace.request_reason
        };
        if cell
            .compare_exchange(0, reason as u8 + 1, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            eprintln!("provider claude diagnostic: {}", reason.label());
        }
    }
    error
}
#[derive(Clone, Copy)]
pub(super) enum Counter {
    Accepted,
    RawRefused,
    RequestRefused,
}
pub(super) fn counter(trace: &Option<Arc<Trace>>, kind: Counter) {
    if let Some(trace) = trace {
        let (cell, name) = match kind {
            Counter::Accepted => (&trace.accepted, "frontend_accepted"),
            Counter::RawRefused => (&trace.raw_refused, "frontend_raw_refused"),
            Counter::RequestRefused => (&trace.request_refused, "frontend_request_refused"),
        };
        if let Ok(previous) = cell.fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
            n.checked_add(1).filter(|n| *n <= 16)
        }) {
            eprintln!("provider claude counter: {name} {}", previous + 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reasons_are_first_per_gate_and_counters_saturate_without_stage_bitmap_growth() {
        let trace = Trace::synthetic();
        let optional = Some(trace.clone());
        for _ in 0..32 {
            counter(&optional, Counter::Accepted);
            counter(&optional, Counter::RawRefused);
            counter(&optional, Counter::RequestRefused);
        }
        assert_eq!(trace.counts(), (16, 16, 16));
        use chimaera_core::provider_runtime::Error;
        assert_eq!(
            refusal(&optional, Refusal::RawEof, Error::InvalidRequest),
            Error::InvalidRequest
        );
        refusal(&optional, Refusal::RawApiKey, Error::InvalidRequest);
        refusal(&optional, Refusal::RequestBeta, Error::InvalidRequest);
        refusal(
            &optional,
            Refusal::RequestAuthMismatch,
            Error::InvalidRequest,
        );
        assert_eq!(
            trace.reasons(),
            (Refusal::RawEof as u8 + 1, Refusal::RequestBeta as u8 + 1)
        );
        assert_eq!(
            refusal(&None, Refusal::RawIo, Error::Unavailable),
            Error::Unavailable
        );
        assert!((Refusal::RequestBeta as u8) < u8::MAX);
    }
}
