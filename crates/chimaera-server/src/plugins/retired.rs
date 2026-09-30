//! Plugins whose job moved into chimaera itself. A retired id is not in the
//! lock, so nothing offers it; a copy an older daemon installed stays
//! listed, so the user can Remove it, with the reason as its `fault`
//! (`resolve`) — never active, its switch refuses on. Every way in refuses
//! the id with a 409 in the same words: by id, by the repository it was
//! released from (before anything is fetched), a release or a local build
//! whose manifest names it, and Update, Use previous and Check of a kept
//! copy. `WorkspaceStore::load` drops the id from every `plugins_on`.

use super::Refusal;

/// A plugin chimaera no longer runs.
pub(crate) struct Retired {
    pub(crate) id: &'static str,
    /// Where it was released: an install from there is refused up front.
    pub(crate) repo: &'static str,
    /// The card's fault, and every refusal's words.
    pub(crate) reason: &'static str,
}

pub(crate) const RETIRED: &[Retired] = &[Retired {
    id: "agent-notes",
    repo: "martinappberg/chimaera-plugin-agent-notes",
    reason: "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy.",
}];

pub(crate) fn of(id: &str) -> Option<&'static Retired> {
    RETIRED.iter().find(|r| r.id == id)
}

/// The retired plugin released from `github` (`owner/repo`, any case).
pub(crate) fn by_repo(github: &str) -> Option<&'static Retired> {
    RETIRED.iter().find(|r| r.repo.eq_ignore_ascii_case(github))
}

impl Retired {
    pub(crate) fn refusal(&self) -> Refusal {
        Refusal::conflict(self.reason)
    }
}

/// `Err` with the 409 every change route answers when `id` is retired.
pub(crate) fn refuse(id: &str) -> Result<(), Refusal> {
    match of(id) {
        Some(r) => Err(r.refusal()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A retired id is never offered: the lock can't name it, and it stays
    /// an id a copy on disk can have (so it is listed and removable).
    #[test]
    fn a_retired_plugin_is_out_of_the_lock_and_a_valid_id() {
        for r in RETIRED {
            assert!(super::super::valid_id(r.id), "{}", r.id);
            assert!(super::super::valid_github(r.repo), "{}", r.repo);
            assert!(
                super::super::lock_entry(r.id).is_none(),
                "{} is retired: it can't be in plugins.lock too",
                r.id
            );
            assert!(
                super::super::lock_entries()
                    .iter()
                    .all(|l| !l.repo.eq_ignore_ascii_case(r.repo)),
                "{}",
                r.repo
            );
        }
        assert_eq!(of("agent-notes").map(|r| r.id), Some("agent-notes"));
        assert!(of("mycelium").is_none());
        assert!(by_repo("MartinAppberg/Chimaera-Plugin-Agent-Notes").is_some());
        assert!(by_repo("acme/agent-notes").is_none());
        let refused = refuse("agent-notes").unwrap_err();
        assert_eq!(refused.status, axum::http::StatusCode::CONFLICT);
        assert_eq!(
            refused.message,
            "Built into Chimaera now: Agent communication (Settings → Agents). Remove this copy."
        );
        assert!(refuse("mycelium").is_ok());
    }
}
