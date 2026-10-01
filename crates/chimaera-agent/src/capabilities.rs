//! Controls implemented by a chat adapter. Catalogs describe the available
//! choices; capabilities describe which operations the host can actually send.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChatCapabilities {
    pub commands: Vec<String>,
    pub image_input: bool,
}

impl ChatCapabilities {
    /// Compatibility for the two original drivers. New adapters must report
    /// their own negotiated capabilities; an unknown identity grants nothing.
    pub fn legacy(kind: &str) -> Self {
        let extra: &[&str] = match kind {
            "claude" => &[
                "permission_destination",
                "set_thinking",
                "set_ultracode",
                "rewind",
                "background_tool",
                "stop_task",
                "get_mcp",
                "set_mcp_enabled",
                "reconnect_mcp",
                "set_remote_control",
            ],
            "codex" => &["compact", "steer_queued"],
            _ => return Self::default(),
        };
        Self {
            commands: [
                "send",
                "permission",
                "permission_feedback",
                "interrupt",
                "set_mode",
                "set_model",
                "set_effort",
                "answer",
                "get_usage",
                "cancel_queued",
                "send_after_turn",
                "send_now",
                "send_if_running",
            ]
            .into_iter()
            .chain(extra.iter().copied())
            .map(str::to_owned)
            .collect(),
            image_input: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_harness_never_inherits_codex_controls() {
        assert_eq!(ChatCapabilities::legacy("pi"), ChatCapabilities::default());
        let claude = ChatCapabilities::legacy("claude");
        let codex = ChatCapabilities::legacy("codex");
        assert!(claude.commands.iter().any(|c| c == "set_thinking"));
        assert!(!codex.commands.iter().any(|c| c == "set_thinking"));
        assert!(codex.commands.iter().any(|c| c == "compact"));
    }
}
