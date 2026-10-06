//! Pure bounded lease verification. This module starts no process, timer or
//! network request; callers must separately negotiate execution capability.
use std::time::{Duration, Instant, SystemTime};

pub(super) const MAX_LEASE: Duration = Duration::from_secs(90);
pub(super) const STOP_MARGIN: Duration = Duration::from_secs(15);
/// How long after a lease's expiry the account still refuses anyone else's
/// takeover (its reconnect grace, `failover_grace_seconds`). A thawed
/// computer may renew before fencing only while this has not passed.
pub(super) const TAKEOVER_GRACE: Duration = Duration::from_secs(15);
/// Kept back from that point for the renewal's own round trip and clock skew.
const TAKEOVER_MARGIN: Duration = Duration::from_secs(5);
const CLOCK_DISCONTINUITY: Duration = Duration::from_secs(1);

#[derive(Clone, Copy)]
pub(in crate::pro) struct RequestStart {
    pub monotonic: Instant,
    pub wall: SystemTime,
}
impl RequestStart {
    pub fn now() -> Self {
        Self {
            monotonic: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}
#[derive(Clone)]
pub(super) struct Deadline {
    start: RequestStart,
    lifetime: Duration,
}
impl Deadline {
    /// Long past: started before any takeover window could still be open.
    #[cfg(any(test, feature = "daemon-extension-fixture"))]
    pub(in crate::pro) fn expired_fixture() -> Self {
        let now = RequestStart::now();
        Self {
            start: RequestStart {
                monotonic: now.monotonic,
                wall: now.wall - MAX_LEASE - STOP_MARGIN - TAKEOVER_GRACE,
            },
            lifetime: Duration::ZERO,
        }
    }
    /// Lapsed a moment ago: the local deadline passed, but the account's
    /// lease and grace have not.
    #[cfg(test)]
    pub(in crate::pro) fn lapsed_recently_fixture() -> Self {
        Self {
            start: RequestStart::now(),
            lifetime: Duration::ZERO,
        }
    }

    pub fn from_response(start: RequestStart, server_remaining: Duration) -> Option<Self> {
        if server_remaining > MAX_LEASE || server_remaining <= STOP_MARGIN {
            return None;
        }
        let deadline = Self {
            start,
            lifetime: server_remaining - STOP_MARGIN,
        };
        deadline.valid().then_some(deadline)
    }
    pub fn valid(&self) -> bool {
        self.valid_at(Instant::now(), SystemTime::now())
    }
    /// Time left, by wall clock, before anyone else could acquire this lease
    /// (expiry plus the account's grace, less a margin); `None` once passed.
    pub fn before_takeover(&self) -> Option<Duration> {
        self.before_takeover_at(SystemTime::now())
    }
    fn before_takeover_at(&self, wall: SystemTime) -> Option<Duration> {
        let elapsed = wall.duration_since(self.start.wall).ok()?;
        (self.lifetime + STOP_MARGIN + TAKEOVER_GRACE)
            .checked_sub(TAKEOVER_MARGIN)?
            .checked_sub(elapsed)
            .filter(|left| !left.is_zero())
    }
    fn valid_at(&self, now: Instant, wall: SystemTime) -> bool {
        let Some(elapsed) = now.checked_duration_since(self.start.monotonic) else {
            return false;
        };
        let Ok(wall_elapsed) = wall.duration_since(self.start.wall) else {
            return false;
        };
        elapsed < self.lifetime && elapsed.abs_diff(wall_elapsed) <= CLOCK_DISCONTINUITY
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authority_shortens_by_request_latency_and_has_stop_margin() {
        let start = RequestStart::now();
        let proof = Deadline::from_response(start, Duration::from_secs(90)).unwrap();
        assert!(proof.valid_at(
            start.monotonic + Duration::from_secs(74),
            start.wall + Duration::from_secs(74)
        ));
        assert!(!proof.valid_at(
            start.monotonic + Duration::from_secs(75),
            start.wall + Duration::from_secs(75)
        ));
        assert!(Deadline::from_response(start, Duration::from_secs(91)).is_none());
        assert!(Deadline::from_response(start, STOP_MARGIN).is_none());
    }
    #[test]
    fn clock_jumps_and_unobserved_suspend_never_extend_authority() {
        let start = RequestStart::now();
        let proof = Deadline::from_response(start, MAX_LEASE).unwrap();
        assert!(!proof.valid_at(
            start.monotonic + Duration::from_secs(1),
            start.wall + Duration::from_secs(10)
        ));
        assert!(!proof.valid_at(
            start.monotonic + Duration::from_secs(10),
            start.wall + Duration::from_secs(1)
        ));
        assert!(!proof.valid_at(
            start.monotonic + Duration::from_secs(1),
            start.wall - Duration::from_secs(1)
        ));
        let old = RequestStart {
            monotonic: start.monotonic - Duration::from_secs(80),
            wall: start.wall - Duration::from_secs(80),
        };
        assert!(Deadline::from_response(old, MAX_LEASE).is_none());
    }
    #[test]
    fn a_thawed_computer_may_renew_first_only_before_anyone_could_take_over() {
        // A 60 s lease: the account lets another holder in 15 s after expiry,
        // so a thaw within 70 s of the request (5 s margin) renews first.
        let start = RequestStart::now();
        let proof = Deadline::from_response(start, Duration::from_secs(60)).unwrap();
        assert_eq!(
            proof.before_takeover_at(start.wall + Duration::from_secs(50)),
            Some(Duration::from_secs(20))
        );
        assert_eq!(
            proof.before_takeover_at(start.wall + Duration::from_secs(69)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            proof.before_takeover_at(start.wall + Duration::from_secs(70)),
            None
        );
        // A wall clock behind the request start proves nothing: no window.
        assert_eq!(
            proof.before_takeover_at(start.wall - Duration::from_secs(1)),
            None
        );
    }
}
