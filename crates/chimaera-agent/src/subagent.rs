//! Reading one subagent's own conversation.
//!
//! A subagent's transcript is hidden from its parent's (both official
//! clients do the same): the parent shows one `Agent:` row and a finished
//! line. This module is the other half — the read-only view of what the
//! subagent itself did, fetched on demand and never journaled.
//!
//! Each agent keeps that conversation somewhere different. Claude writes
//! `subagents/agent-<id>.jsonl` beside the parent's transcript
//! ([`crate::transcript::import_subagent_transcript`], read by the daemon);
//! codex holds it as a real child thread that only its app-server connection
//! can read, so the request travels to the live driver as a [`DriverQuery`].

use tokio::sync::oneshot;

use crate::model::{truncate_label, AgentEvent};

/// Outstanding [`DriverQuery`]s a driver accepts at once (queued + in
/// flight). A view polls one transcript at a time; past this the request is
/// refused rather than queued without bound.
pub const QUERY_QUEUE: usize = 4;
/// One model id on a `SubagentInfo` / transcript, capped at construction.
pub const SUBAGENT_MODEL_MAX: usize = 128;
/// One subagent handle (`agent_id`) or kind name, capped at construction.
pub const SUBAGENT_ID_MAX: usize = 128;

/// One subagent's conversation as normalized events — the same stream a chat
/// of its own would have produced.
#[derive(Debug, Default)]
pub struct SubagentTranscript {
    /// The retained tail of the conversation, oldest first.
    pub events: Vec<AgentEvent>,
    /// Names the window `events` starts at. While it is unchanged, a later
    /// read returns the same events plus whatever was added, so a reader may
    /// append the difference; when it changes the reader starts over.
    pub epoch: String,
    /// The model serving the subagent, when the source names one.
    pub model: Option<String>,
    /// When each of `events` happened (epoch ms; 0 = not known), parallel
    /// to `events`. Empty when the source records no times at all.
    pub timestamps: Vec<u64>,
}

/// A read the daemon asks of a LIVE driver. Ephemeral: the answer goes back
/// on `reply` and nothing enters the journal.
pub enum DriverQuery {
    /// The conversation of the subagent the driver knows as `agent_id`.
    SubagentTranscript {
        agent_id: String,
        /// The subagent is still working: leave its open turn open.
        live: bool,
        reply: oneshot::Sender<Result<SubagentTranscript, String>>,
    },
}

impl DriverQuery {
    /// Answer with a refusal (an agent with no such read, a full queue).
    pub fn refuse(self, reason: &str) {
        match self {
            Self::SubagentTranscript { reply, .. } => {
                let _ = reply.send(Err(reason.to_string()));
            }
        }
    }
}

/// Fold one fact the wire names about a subagent into `slot` — latest wins,
/// capped at construction — and say whether it is news. An empty value
/// teaches nothing (a field the wire has not named yet); callers strip
/// their own placeholders first. Shared by both drivers so the two cannot
/// drift on what counts as a change.
pub fn learn_label(slot: &mut Option<String>, value: Option<&str>, cap: usize) -> bool {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return false;
    };
    let value = truncate_label(value, cap);
    if slot.as_deref() == Some(value.as_str()) {
        return false;
    }
    *slot = Some(value);
    true
}

/// Whether `id` is safe to name a subagent with: it is put in a file name
/// (claude) and sent as a thread id (codex), so only the characters both
/// agents mint — hex, UUID dashes, underscores.
pub fn valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= SUBAGENT_ID_MAX
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_cannot_leave_their_directory() {
        assert!(valid_agent_id("a0dd2a5017a275850"));
        assert!(valid_agent_id("01a10e1a-9700-7682-87cf-eb8448a31ad6"));
        for bad in ["", "..", "a/b", "a.jsonl", "a b", "a\0"] {
            assert!(!valid_agent_id(bad), "{bad:?}");
        }
        assert!(!valid_agent_id(&"a".repeat(SUBAGENT_ID_MAX + 1)));
    }
}
