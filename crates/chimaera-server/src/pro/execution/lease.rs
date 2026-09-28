//! Pure bounded lease verification. This module starts no process, timer or
//! network request; callers must separately negotiate execution capability.
use std::time::{Duration, Instant, SystemTime};

pub(super) const MAX_LEASE: Duration = Duration::from_secs(90);
pub(super) const STOP_MARGIN: Duration = Duration::from_secs(15);
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
    #[cfg(test)]
    pub(in crate::pro) fn expired_fixture() -> Self {
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
}
