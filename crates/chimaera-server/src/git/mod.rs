//! Git integration: status, diff, and worktree orchestration for a workspace's
//! repo.
//!
//! Shells out to system `git` and parses porcelain v2 (docs/design/README.md "Git + Slurm":
//! gitoxide's diff gaps make a library a two-backend liability; shelling out is
//! adequate for read-mostly status/log/diff/show). Every invocation is bounded
//! because the daemon shares a login node (docs/design/README.md resource budget): a hard
//! timeout that KILLS the child (a wedged NFS mount must never pin a thread), an
//! output-size cap, an entry-count cap, and a daemon-wide concurrency permit.
//!
//! Inspection is read-only and stores nothing durable: git state is
//! reconstructible, so status and diffs are recomputed on demand (the
//! per-session anchors in `session` are bounded, in-memory). The ONLY
//! mutations are worktree create/remove/lock/unlock, and they are confined
//! to the managed root (`AppState::worktrees_root`) — chimaera never removes
//! a checkout it did not create, never one a live session is sitting in,
//! never one with uncommitted or unshared work unless forced, and never
//! touches a lock another tool made. It never commits, checks out, resets,
//! rebases, pushes or merges.

pub(crate) mod anchor;
mod history;
mod http;
mod include;
mod parse;
mod repos;
mod resolve;
mod rev;
mod service;
mod session;
mod worktree;

pub(crate) use history::{compare, log, show};
pub(crate) use http::{branches, diff, repos, status, worktrees};
pub(crate) use repos::note_listed_dir;
pub(crate) use service::{
    backstop_poll, git_facts, mark_path_dirty, usable_git_dir, GitService, WatchGuard,
};
pub(crate) use session::{session_git, session_turn_end, track_sessions};
pub(crate) use worktree::{
    allowed_session_cwd, create_worktree, ensure_branch_worktree, remove_worktree,
};
