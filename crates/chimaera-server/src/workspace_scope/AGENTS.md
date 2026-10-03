# Workspace HTTP authority

The parent [daemon map](../../AGENTS.md) owns route wiring and the public wire.
This directory supports `workspace_scope.rs`: an authenticated forwarded request
is admitted for one exact project, epoch and account generation. It does not
wake or acquire execution.

| File | Responsibility |
|---|---|
| `commands.rs` | Body admission and owned lifecycle dispatch. The original captured reservation survives cancellation; a delayed body cannot adopt a new epoch. |
| `paths.rs` | Explicit viewer-to-owner path aliases and metadata response mapping. File bytes and arbitrary JSON/text contents are never rewritten. |
| `files.rs` | Literal no-follow project-root pinning, single resolution of intentional internal links, descriptor-relative file/parent capabilities, and authority rechecks. This project's saved session uploads remain beneath a captured daemon-state root and require the current exact session association. |
| `files/consumer_tests.rs` | Real HTTP consumer regressions: parent replacement cannot read or mutate another project, internal links and nested mutations still work, scoped tickets retain their original authority; saved-image aliases remain readable and upload cancellation drains its own temporary. |

Filesystem proof is attached to descriptors, not a canonical path string passed
to a later absolute-path open. Every ancestor of the registered root is opened
without following links; its device/inode must still match. Intentional internal
symlinks resolve once, then the actual open walks their resolved relative path
with `O_NOFOLLOW`. Leaf rename/delete acts on the link itself. Saved images
accept the configured daemon-state parent's OS aliases (for example `/var` on
macOS) at admission; the captured canonical state-root descriptor then pins
literal `uploads/<session>` descendants. Replacing an upload/session ancestor
cannot authorize a new outside root. Ordinary session uploads derive their
landing pad from registered sessions in trusted daemon state and retain their
existing behavior.

`Context` captures account generation and project epoch at admission. Blocking
consumers recheck that identity and the registered root before opening or
publishing a filesystem effect. Owned mutation reservations still govern writes;
reads do not consume mutation capacity. `fs/scoped.rs` handles file writes and
recursive tree effects through verified parent descriptors. Walks retain the
existing 250,000-entry ceiling plus a 128-level recursion ceiling. A failed
scoped copy retains its partial destination rather than deleting concurrent
user changes. Rename publication uses `NOREPLACE`; case-only changes on a
case-insensitive filesystem use an exclusive temporary hop with non-replacing
rollback (or an actionable retained original). Cross-device moves fingerprint
the whole source tree's identity, metadata and content before copying and
before deletion; changes during copying retain both copies. These checks do
not provide atomic exclusion against arbitrary external filesystem writers.

Scoped raw/download tickets retain their descriptor-rooted proof and cannot be
renewed from an unrestricted or another epoch's ticket. Unchanged previews
retain stable URLs only within the exact account generation, project epoch,
registered/captured root identity, path and file version; this cache uses no
filesystem I/O under the ticket-store lock. Raw HTML assets remain
within the ticket's folder, with the existing visible-component and sandbox
rules. Epoch/account replacement invalidates the old capability even when the
subsequent ticket request carries no scope headers. Path, remaining TTL and
optional scoped authority are captured together under one ticket-store lock;
expiry/eviction can never turn a missing scoped capability into a plain path.

Filesystem observer and scoped-effect work slots belong to the actual blocking
workers, including after HTTP cancellation. Scoped directory uploads reserve a
bounded slot before detaching their owned stream; body streaming holds no
execution reservation. Their hidden temporary file remains descriptor-bound,
and only final publication acquires the original epoch's mutation reservation.
Body reads have a 60-second idle and 30-minute total stream deadline, including
for a canceled observer; a stalled body drains its own temporary and slot.
Cleanup removes only the inode created by that upload, and drains before a
normal response. Unrestricted daemon requests omit `Context` and retain their
existing local filesystem semantics.
