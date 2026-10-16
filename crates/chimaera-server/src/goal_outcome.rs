//! How a Claude `/goal` ended. Its check runs at each stop, but the verdict
//! that lets the agent stop — achieved, or could not be achieved — never
//! reaches stdout: only the native transcript records it, as a
//! `goal_status` attachment (live 2.1.300, PROTOCOL.md Pass 46). After a turn
//! ends with a goal still active, this reads the transcript's tail for the
//! goal's latest record and journals the end (`ChatManager::annotate`). A
//! record that still says "not met" (the stop was let through by the block
//! cap, or the check timed out) leaves the goal set.

use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use chimaera_agent::model::{truncate_label, AgentEvent, GoalEnd, GOAL_REASON_MAX, GOAL_TEXT_MAX};

use crate::agents::AgentKind;
use crate::AppState;

/// The tail read per attempt: a turn's own records sit at the end, and
/// 2.1.300 writes large per-request records, so this is generous but bounded.
const TAIL_BYTES: u64 = 1024 * 1024;
/// The transcript can trail the `result` frame; the record is usually there
/// at once (live: written ~1.5 s before the result).
const ATTEMPTS_MS: [u64; 3] = [250, 1_500, 4_000];

/// Each Claude chat's active goal objective, as its events said. Owned by
/// the chat signal task.
#[derive(Default)]
pub(crate) struct GoalWatch {
    active: HashMap<String, String>,
}

impl GoalWatch {
    pub(crate) fn observe(&mut self, state: &Arc<AppState>, id: &str, ev: &AgentEvent) {
        match ev {
            AgentEvent::Goal { goal: Some(g), .. } if g.status == "active" => {
                let claude = crate::lock(&state.agents)
                    .get(id)
                    .is_some_and(|r| r.kind == AgentKind::Claude);
                if claude {
                    self.active.insert(id.to_string(), g.objective.clone());
                }
            }
            AgentEvent::Goal { .. } | AgentEvent::Exited { .. } => {
                self.active.remove(id);
            }
            AgentEvent::TurnCompleted { .. } => {
                if let Some(objective) = self.active.get(id) {
                    tokio::spawn(resolve(
                        Arc::clone(state),
                        id.to_string(),
                        objective.clone(),
                    ));
                }
            }
            _ => {}
        }
    }
}

enum Verdict {
    Ended(GoalEnd),
    StillSet,
}

async fn resolve(state: Arc<AppState>, id: String, objective: String) {
    let Some(info) = state.chat.get(&id) else {
        return;
    };
    let Some(native) = info.native_session_id.clone() else {
        return;
    };
    let recorded = crate::claude_store::recorded_for(&state, &native);
    let Some(path) = crate::claude_store::find(
        state.claude_projects_dir.clone(),
        native,
        recorded,
        vec![info.cwd.clone()],
    )
    .await
    else {
        return;
    };
    for delay in ATTEMPTS_MS {
        tokio::time::sleep(Duration::from_millis(delay)).await;
        let (p, o) = (path.clone(), objective.clone());
        let verdict = tokio::task::spawn_blocking(move || read_verdict(&p, &o))
            .await
            .ok()
            .flatten();
        match verdict {
            Some(Verdict::Ended(end)) => {
                // Only if that goal is still the one set: a new `/goal` (or a
                // clear) in the meantime has its own events.
                let current = state.chat.get(&id).and_then(|i| i.goal);
                let same = current
                    .is_some_and(|g| g.status == "active" && same_goal(&g.objective, &objective));
                if same {
                    if let Err(err) = state.chat.annotate(
                        &id,
                        AgentEvent::Goal {
                            goal: None,
                            ended: Some(end),
                        },
                    ) {
                        tracing::debug!(%id, %err, "goal end not journaled");
                    }
                }
                return;
            }
            Some(Verdict::StillSet) => return,
            None => {}
        }
    }
}

/// `brief` is the row's capped copy; compare the part both have.
fn same_goal(brief: &str, objective: &str) -> bool {
    let brief = brief.strip_suffix('…').unwrap_or(brief);
    objective.starts_with(brief)
}

/// The goal's latest `goal_status` record in the transcript's tail, or
/// `None` when it is not there (yet). Blocking.
fn read_verdict(path: &Path, objective: &str) -> Option<Verdict> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut tail = Vec::with_capacity((len - start) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut tail).ok()?;
    let text = String::from_utf8_lossy(&tail);
    // Our copy may be capped; the transcript's is whole.
    let ours = objective.strip_suffix('…').unwrap_or(objective);
    for line in text.lines().rev() {
        if !line.contains("\"goal_status\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let status = &record["attachment"];
        if status["type"] != "goal_status" {
            continue;
        }
        let condition = status["condition"].as_str().unwrap_or_default();
        if !condition.starts_with(ours) {
            continue;
        }
        let outcome = if status["met"] == true {
            "achieved"
        } else if status["failed"] == true {
            "failed"
        } else {
            return Some(Verdict::StillSet);
        };
        return Some(Verdict::Ended(GoalEnd {
            outcome: outcome.into(),
            objective: truncate_label(condition, GOAL_TEXT_MAX),
            reason: status["reason"]
                .as_str()
                .map(|r| truncate_label(r.trim(), GOAL_REASON_MAX))
                .filter(|r| !r.is_empty()),
            turns: status["iterations"]
                .as_u64()
                .map(|n| n.min(u32::MAX as u64) as u32),
            tokens: status["tokens"].as_u64(),
            duration_secs: status["durationMs"].as_u64().map(|ms| ms / 1000),
        }));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A transcript file that removes itself.
    struct Transcript(std::path::PathBuf);

    impl Transcript {
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Transcript {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn transcript(lines: &[serde_json::Value]) -> Transcript {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("chimaera-goal-{}-{n}.jsonl", std::process::id()));
        let body: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(&path, body).unwrap();
        Transcript(path)
    }

    fn status(attachment: serde_json::Value) -> serde_json::Value {
        json!({"type": "attachment", "attachment": attachment})
    }

    #[test]
    fn the_latest_record_for_the_goal_decides() {
        let cond = "the file n.txt contains the number 3";
        let file = transcript(&[
            status(
                json!({"type": "goal_status", "met": false, "sentinel": true, "condition": cond}),
            ),
            status(
                json!({"type": "goal_status", "met": false, "condition": cond, "reason": "only 1"}),
            ),
            json!({"type": "assistant", "message": {"content": []}}),
            status(
                json!({"type": "goal_status", "met": true, "condition": cond,
                "reason": "n.txt now holds 3", "iterations": 3, "durationMs": 21_500, "tokens": 41_000}),
            ),
        ]);
        let Some(Verdict::Ended(end)) = read_verdict(file.path(), cond) else {
            panic!("ended");
        };
        assert_eq!(end.outcome, "achieved");
        assert_eq!(end.reason.as_deref(), Some("n.txt now holds 3"));
        assert_eq!(
            (end.turns, end.tokens, end.duration_secs),
            (Some(3), Some(41_000), Some(21))
        );
    }

    #[test]
    fn could_not_be_achieved_and_still_set_and_absent() {
        let cond = "x";
        let failed = transcript(&[status(json!({"type": "goal_status", "met": false,
            "failed": true, "condition": cond, "reason": "impossible"}))]);
        let Some(Verdict::Ended(end)) = read_verdict(failed.path(), cond) else {
            panic!("ended");
        };
        assert_eq!(end.outcome, "failed");
        let not_met = transcript(&[status(json!({"type": "goal_status", "met": false,
            "condition": cond, "reason": "not yet"}))]);
        assert!(matches!(
            read_verdict(not_met.path(), cond),
            Some(Verdict::StillSet)
        ));
        // Another goal's record, or none at all, says nothing (yet).
        let other = transcript(&[status(
            json!({"type": "goal_status", "met": true, "condition": "y"}),
        )]);
        assert!(read_verdict(other.path(), cond).is_none());
    }

    #[test]
    fn a_capped_copy_matches_its_whole_goal() {
        assert!(same_goal("make the tests pa…", "make the tests pass"));
        assert!(!same_goal("make the docs…", "make the tests pass"));
    }
}
