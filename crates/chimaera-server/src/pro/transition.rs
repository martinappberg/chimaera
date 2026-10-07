//! The one write path for a project's ownership, and the transition log.
//!
//! Every ownership change goes through [`apply`] (or [`set_ownership`]),
//! which writes one info line (target `chimaera_server::pro::transition`:
//! workspace id, epoch, from, to, reason) only when the value changes, and
//! ends the project's transfer deadline (`moving`) when the change settles
//! it. Deadline hits and refused hand-backs log one line each here too, so a
//! stalled transfer reads from one target. Workspace ids are opaque; no path,
//! name or error text is logged.
use super::Ownership;
use crate::{lock, AppState};
use std::collections::HashMap;

const TARGET: &str = "chimaera_server::pro::transition";

fn name(owner: Option<&Ownership>) -> &'static str {
    match owner {
        None => "none",
        Some(Ownership::PrivacyDisabled { .. }) => "privacy_disabled",
        Some(Ownership::Hydrating { .. }) => "hydrating",
        Some(Ownership::SettingUp { .. }) => "setting_up",
        Some(Ownership::AwaitingVerification { .. }) => "awaiting_verification",
        Some(Ownership::Local { .. }) => "local",
        Some(Ownership::Remote { .. }) => "remote",
        Some(Ownership::Transferring { .. }) => "transferring",
    }
}

pub(super) fn epoch(owner: Option<&Ownership>) -> Option<u64> {
    owner.map(|owner| match owner {
        Ownership::PrivacyDisabled { epoch }
        | Ownership::Hydrating { epoch }
        | Ownership::SettingUp { epoch }
        | Ownership::AwaitingVerification { epoch }
        | Ownership::Local { epoch }
        | Ownership::Remote { epoch, .. }
        | Ownership::Transferring { epoch } => *epoch,
    })
}

/// The one info line for an ownership change (also the boot remap, which
/// runs before there is state to [`apply`] to).
pub(super) fn log(
    workspace: &str,
    old: Option<&Ownership>,
    new: Option<&Ownership>,
    reason: &'static str,
) {
    tracing::info!(
        target: TARGET,
        workspace,
        epoch = epoch(new.or(old)),
        from = name(old),
        to = name(new),
        reason,
        "ownership"
    );
}

/// Writes `new` into an ownership map the caller already holds (so a check
/// and its write stay one critical section). Takes no lock but the transfer
/// record's own, which is never held while ownership is taken.
pub(super) fn apply(
    state: &AppState,
    ownership: &mut HashMap<String, Ownership>,
    workspace: &str,
    new: Option<Ownership>,
    reason: &'static str,
) {
    let old = ownership.get(workspace).cloned();
    if old == new {
        return;
    }
    log(workspace, old.as_ref(), new.as_ref(), reason);
    // Held here, gone, or newly elsewhere: whatever transfer was under way
    // has ended. Remote to Remote (a return still waiting) and every fence
    // in between leave the deadline running.
    let settled = match &new {
        None | Some(Ownership::Local { .. }) => true,
        Some(Ownership::Remote { .. }) => !matches!(old, Some(Ownership::Remote { .. })),
        _ => false,
    };
    match new {
        Some(value) => {
            ownership.insert(workspace.to_owned(), value);
        }
        None => {
            ownership.remove(workspace);
        }
    }
    if settled {
        super::moving::end(state, workspace);
    }
}

/// [`apply`] under its own lock.
pub(super) fn set_ownership(
    state: &AppState,
    workspace: &str,
    new: Option<Ownership>,
    reason: &'static str,
) {
    apply(
        state,
        &mut lock(&state.pro.ownership),
        workspace,
        new,
        reason,
    );
}

/// A transfer reached its deadline (`moving::transfer_deadline`).
pub(super) fn deadline(workspace: &str, epoch: Option<u64>, what: &'static str) {
    tracing::info!(target: TARGET, workspace, epoch, what, "deadline");
}

/// A pause decision refused a hand-back: the conversation is still working.
pub(super) fn refused(workspace: &str, epoch: u64, reason: &'static str) {
    tracing::info!(target: TARGET, workspace, epoch, reason, "hand-back refused");
}
