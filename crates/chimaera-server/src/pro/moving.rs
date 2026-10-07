//! One deadline for every transfer of a project between this computer and
//! the cloud (or another of the user's computers).
//!
//! Each transfer records when it was triggered (`move_started_ms`, wall
//! clock on purpose: it is persisted in `state.json` and must survive a
//! restart) at the sleep flush, "Run in the cloud", "Run here", the first
//! live attempt of an automatic return and a move between computers. The
//! record ends when ownership settles the project here or elsewhere
//! (`transition::apply`). [`transfer_deadline`] is the only policy: past it,
//! a transfer to the cloud nobody took comes back here, and a return the
//! cloud has not released is requested from the account and taken with the
//! last checkpoint once the cloud's lease lapses.
use crate::{lock, AppState};
use std::time::Duration;

/// A transfer settles within this of its trigger, or for a return of a
/// conversation the cloud was running, of the cloud's pause (the pause itself
/// is bounded by the turn, with no cap of its own).
const DEADLINE_MS: u64 = 180_000;
/// The same bound for a wait measured in time rather than read off the clock
/// (a move between computers, a device's stop after another owner).
pub(super) const DEADLINE: Duration = Duration::from_millis(DEADLINE_MS);
/// While the cloud's conversation is mid-turn it is asked again this often:
/// the return follows its pause within a few seconds.
pub(super) const BUSY_RETRY_SECS: u64 = 5;
/// A failed attempt is tried again after this long.
pub(super) const RETRY_SECS: u64 = 20;
/// A restore (this computer already holds the lease) is tried this many
/// times; then the project stays here with the files it has and the cloud's
/// changes are kept for review.
pub(super) const RESTORE_TRIES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Deadline {
    Patience,
    Overdue,
}

pub(super) fn transfer_deadline(now_ms: u64, started_ms: u64) -> Deadline {
    if now_ms.saturating_sub(started_ms) >= DEADLINE_MS {
        Deadline::Overdue
    } else {
        Deadline::Patience
    }
}

pub(super) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Records a transfer's trigger; an earlier trigger for the same transfer
/// stands. Returns whether this started one. The caller persists.
pub(super) fn begin(state: &AppState, workspace: &str) -> bool {
    let mut moving = lock(&state.pro.moving);
    if moving.contains_key(workspace) || moving.len() >= 128 {
        return false;
    }
    moving.insert(workspace.to_owned(), now_ms());
    true
}

pub(super) fn started(state: &AppState, workspace: &str) -> Option<u64> {
    lock(&state.pro.moving).get(workspace).copied()
}

pub(super) fn end(state: &AppState, workspace: &str) {
    lock(&state.pro.moving).remove(workspace);
    lock(&state.pro.return_backoff).remove(workspace);
}

/// Where an automatic return stands between passes. Hot state: a restart
/// asks the cloud again before deciding anything.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Retry {
    /// No attempt before this (unix seconds).
    pub next: u64,
    /// Failed restores so far (`RESTORE_TRIES`).
    pub tries: u8,
    /// When the cloud last said its conversation is mid-turn (unix ms; 0:
    /// never): the return's deadline runs from its pause, not its trigger.
    pub busy_ms: u64,
    /// The account was asked to move the project here (once per transfer).
    pub requested: bool,
}

impl Retry {
    /// Back home: the cloud's pause, or the trigger when it never said busy.
    pub fn deadline(&self, started_ms: u64, now_ms: u64) -> Deadline {
        transfer_deadline(now_ms, started_ms.max(self.busy_ms))
    }
}

pub(super) fn retry(state: &AppState, workspace: &str) -> Retry {
    lock(&state.pro.return_backoff)
        .get(workspace)
        .copied()
        .unwrap_or_default()
}

pub(super) fn set_retry(state: &AppState, workspace: &str, retry: Retry) {
    let mut backoff = lock(&state.pro.return_backoff);
    if backoff.len() >= 128 && !backoff.contains_key(workspace) {
        backoff.clear();
    }
    backoff.insert(workspace.to_owned(), retry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_deadline_for_every_transfer() {
        assert_eq!(transfer_deadline(1_000, 1_000), Deadline::Patience);
        assert_eq!(
            transfer_deadline(1_000 + DEADLINE_MS - 1, 1_000),
            Deadline::Patience
        );
        assert_eq!(
            transfer_deadline(1_000 + DEADLINE_MS, 1_000),
            Deadline::Overdue
        );
        // A clock that went back never reads overdue.
        assert_eq!(transfer_deadline(0, 5_000), Deadline::Patience);
        // Back home runs from the cloud's pause, not the trigger.
        let retry = Retry {
            busy_ms: 100_000,
            ..Default::default()
        };
        assert_eq!(
            retry.deadline(0, 100_000 + DEADLINE_MS - 1),
            Deadline::Patience
        );
        assert_eq!(retry.deadline(0, 100_000 + DEADLINE_MS), Deadline::Overdue);
        assert_eq!(DEADLINE.as_secs(), 180);
    }
}
