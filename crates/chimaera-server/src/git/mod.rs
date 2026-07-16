//! Git integration: status, diff, and worktree orchestration for a workspace's
//! repo.
//!
//! Shells out to system `git` and parses porcelain v2 (DESIGN.md "Git + Slurm":
//! gitoxide's diff gaps make a library a two-backend liability; shelling out is
//! adequate for read-mostly status/log/diff/show). Every invocation is bounded
//! because the daemon shares a login node (DESIGN.md resource budget): a hard
//! timeout that KILLS the child (a wedged NFS mount must never pin a thread), an
//! output-size cap, an entry-count cap, and a daemon-wide concurrency permit.
//!
//! Inspection is read-only and stores nothing: git state is reconstructible, so
//! status and diffs are recomputed on demand. The ONLY mutations are worktree
//! create/remove, and they are confined to the managed root
//! (`AppState::worktrees_root`) — chimaera never removes a checkout it did not
//! create, never one a live session is sitting in, and never one with
//! uncommitted work unless forced.

mod http;
mod parse;
mod resolve;
mod service;
mod worktree;

pub(crate) use http::{diff, status, worktrees};
pub(crate) use service::{backstop_poll, git_facts, mark_path_dirty, GitService, WatchGuard};
pub(crate) use worktree::{create_worktree, remove_worktree};
