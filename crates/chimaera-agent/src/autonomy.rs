//! Claude's self-resuming work, derived from its frames: the pending
//! `ScheduleWakeup` and the session jobs (`CronCreate`) that `/loop` makes,
//! and the `/goal` the CLI checks each time the agent stops. Claude has no
//! control request that lists or reports any of it (PROTOCOL.md Pass 46), so
//! the driver keeps this state from the tools' calls and structured results,
//! the `/goal` command's own replies, and the goal check's feedback frames.
//! Pure state: the driver feeds frames in and journals what comes out.

use std::collections::HashMap;

use serde_json::Value;

use crate::cron::Cron;
use crate::model::{
    truncate_label, AgentEvent, GoalEnd, GoalState, ScheduledJob, ScheduledWakeup, GOAL_REASON_MAX,
    GOAL_TEXT_MAX, SCHEDULE_JOBS_CAP, SCHEDULE_TEXT_MAX,
};

/// Calls awaiting their result. Each is one model step's tool call, so a
/// handful at most; the bound only stops a hostile stream from growing it.
const PENDING_CAP: usize = 16;
/// Claude expires a recurring session job 7 days after it was made.
const RECURRING_LIFETIME_MS: u64 = 7 * 24 * 3600 * 1000;
/// How early a fire may come: a wakeup fired ~3 s before `scheduledFor`
/// live, and Claude lets a one-shot job fire up to 90 s early.
const EARLY_FIRE_MS: u64 = 120_000;

/// What fired a turn the CLI started by itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fired {
    /// `wakeup` | `job`.
    pub kind: &'static str,
    pub detail: String,
    pub schedule: Option<String>,
}

enum Pending {
    Wakeup {
        reason: String,
        prompt: String,
    },
    Stop,
    Create {
        cron: String,
        prompt: String,
        recurring: bool,
    },
    Delete {
        id: String,
    },
    List,
}

struct Job {
    job: ScheduledJob,
    cron: Option<Cron>,
    /// Claude's stable per-job jitter, so later runs land where it puts them.
    offset_ms: u64,
}

/// The wakeup and the jobs, as the tool results said them.
#[derive(Default)]
pub struct ScheduleTracker {
    wakeup: Option<ScheduledWakeup>,
    jobs: Vec<Job>,
    pending: HashMap<String, Pending>,
}

impl ScheduleTracker {
    pub fn is_schedule_tool(name: &str) -> bool {
        matches!(
            name,
            "ScheduleWakeup" | "CronCreate" | "CronDelete" | "CronList"
        )
    }

    /// Jobs carried over from the previous process: `--resume` restores
    /// them (a wakeup does not survive, so none is carried).
    pub fn seed(&mut self, jobs: Vec<ScheduledJob>, now_ms: u64) {
        self.jobs = jobs.into_iter().map(Job::from_carried).collect();
        self.expire(now_ms);
    }

    pub fn is_empty(&self) -> bool {
        self.wakeup.is_none() && self.jobs.is_empty()
    }

    pub fn on_tool_use(&mut self, id: &str, name: &str, input: &Value) {
        if self.pending.len() >= PENDING_CAP {
            return;
        }
        let text =
            |key: &str| truncate_label(input[key].as_str().unwrap_or_default(), SCHEDULE_TEXT_MAX);
        let call = match name {
            "ScheduleWakeup" if input["stop"] == Value::Bool(true) => Pending::Stop,
            "ScheduleWakeup" => Pending::Wakeup {
                reason: text("reason"),
                prompt: text("prompt"),
            },
            "CronCreate" => Pending::Create {
                cron: truncate_label(input["cron"].as_str().unwrap_or_default(), 256),
                prompt: text("prompt"),
                // The live model has sent `"true"` as a string; Claude still
                // made a recurring job, and the result's own `recurring` wins.
                recurring: !matches!(input["recurring"], Value::Bool(false)),
            },
            "CronDelete" => Pending::Delete {
                id: truncate_label(input["id"].as_str().unwrap_or_default(), 64),
            },
            "CronList" => Pending::List,
            _ => return,
        };
        self.pending.insert(id.to_string(), call);
    }

    pub fn tracks(&self, id: &str) -> bool {
        self.pending.contains_key(id)
    }

    /// A tracked call's result. `result` is the frame's `tool_use_result`
    /// (absent when one frame carried several results). Returns the new
    /// schedule when it changed.
    pub fn on_tool_result(
        &mut self,
        id: &str,
        failed: bool,
        result: Option<&Value>,
        now_ms: u64,
    ) -> Option<AgentEvent> {
        let call = self.pending.remove(id)?;
        if failed {
            return None;
        }
        let result = result.unwrap_or(&Value::Null);
        match call {
            Pending::Wakeup { reason, prompt } => {
                let at_ms = as_ms(&result["scheduledFor"])?;
                self.wakeup = Some(ScheduledWakeup {
                    at_ms,
                    scheduled_at_ms: now_ms,
                    reason,
                    prompt,
                });
            }
            Pending::Stop => {
                self.wakeup.take()?;
            }
            Pending::Create {
                cron,
                prompt,
                recurring,
            } => {
                let id = result["id"].as_str().filter(|id| !id.is_empty())?;
                let job = ScheduledJob {
                    id: truncate_label(id, 64),
                    cron,
                    schedule: truncate_label(
                        result["humanSchedule"].as_str().unwrap_or_default(),
                        SCHEDULE_TEXT_MAX,
                    ),
                    prompt,
                    recurring: result["recurring"].as_bool().unwrap_or(recurring),
                    next_at_ms: as_ms(&result["nextFireAt"]),
                    created_at_ms: now_ms,
                };
                self.jobs.retain(|j| j.job.id != job.id);
                if self.jobs.len() >= SCHEDULE_JOBS_CAP {
                    self.jobs.remove(0);
                }
                self.jobs.push(Job::new(job));
            }
            Pending::Delete { id } => {
                let id = result["id"].as_str().unwrap_or(&id).to_string();
                let before = self.jobs.len();
                self.jobs.retain(|j| j.job.id != id);
                if self.jobs.len() == before {
                    return None;
                }
            }
            Pending::List => {
                let listed = result["jobs"].as_array()?;
                let mut jobs = Vec::new();
                for entry in listed.iter().take(SCHEDULE_JOBS_CAP) {
                    let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) else {
                        continue;
                    };
                    let known = self.jobs.iter().find(|j| j.job.id == id);
                    let job = ScheduledJob {
                        id: truncate_label(id, 64),
                        cron: truncate_label(entry["cron"].as_str().unwrap_or_default(), 256),
                        schedule: truncate_label(
                            entry["humanSchedule"].as_str().unwrap_or_default(),
                            SCHEDULE_TEXT_MAX,
                        ),
                        prompt: truncate_label(
                            entry["prompt"].as_str().unwrap_or_default(),
                            SCHEDULE_TEXT_MAX,
                        ),
                        recurring: entry["recurring"].as_bool().unwrap_or(false),
                        next_at_ms: as_ms(&entry["nextFireAt"]),
                        created_at_ms: known.map_or(now_ms, |j| j.job.created_at_ms),
                    };
                    jobs.push(match known {
                        // Keep the jitter learned at creation: a listed
                        // `nextFireAt` is the same slot plus the same offset.
                        Some(k) if k.job.cron == job.cron => Job {
                            job,
                            cron: k.cron,
                            offset_ms: k.offset_ms,
                        },
                        _ => Job::new(job),
                    });
                }
                self.jobs = jobs;
            }
        }
        Some(self.snapshot())
    }

    /// A turn the CLI started by itself (no message of ours opened it):
    /// the earliest item that is due is what fired. A wakeup is spent; a
    /// recurring job moves to its next run, a one-shot job is gone. `None`
    /// when nothing was due — a monitor event, finished background work, or
    /// a wakeup Claude armed without a frame (its `/loop` keepalive).
    pub fn on_cli_turn(&mut self, now_ms: u64) -> Option<(Fired, AgentEvent)> {
        let horizon = now_ms.saturating_add(EARLY_FIRE_MS);
        let wake_at = self
            .wakeup
            .as_ref()
            .map(|w| w.at_ms)
            .filter(|at| *at <= horizon);
        let job = self
            .jobs
            .iter()
            .enumerate()
            .filter_map(|(i, j)| {
                j.job
                    .next_at_ms
                    .filter(|at| *at <= horizon)
                    .map(|at| (i, at))
            })
            .min_by_key(|(_, at)| *at);
        let fired = match (wake_at, job) {
            (Some(w), Some((i, j))) if j < w => self.fire_job(i, now_ms),
            (Some(_), _) => {
                let wakeup = self.wakeup.take()?;
                Fired {
                    kind: "wakeup",
                    detail: wakeup.reason,
                    schedule: None,
                }
            }
            (None, Some((i, _))) => self.fire_job(i, now_ms),
            (None, None) => return None,
        };
        self.expire(now_ms);
        Some((fired, self.snapshot()))
    }

    fn fire_job(&mut self, index: usize, now_ms: u64) -> Fired {
        let entry = &mut self.jobs[index];
        let fired = Fired {
            kind: "job",
            detail: entry.job.prompt.clone(),
            schedule: Some(entry.job.schedule.clone()).filter(|s| !s.is_empty()),
        };
        if entry.job.recurring {
            entry.job.next_at_ms = entry
                .cron
                .as_ref()
                .and_then(|cron| cron.next_run_ms(now_ms, entry.offset_ms));
        } else {
            self.jobs.remove(index);
        }
        fired
    }

    fn expire(&mut self, now_ms: u64) {
        self.jobs.retain(|j| {
            !j.job.recurring || now_ms.saturating_sub(j.job.created_at_ms) < RECURRING_LIFETIME_MS
        });
    }

    pub fn jobs(&self) -> Vec<ScheduledJob> {
        self.jobs.iter().map(|j| j.job.clone()).collect()
    }

    pub fn snapshot(&self) -> AgentEvent {
        AgentEvent::Schedule {
            wakeup: self.wakeup.clone(),
            jobs: self.jobs(),
        }
    }
}

impl Job {
    fn new(job: ScheduledJob) -> Self {
        let cron = crate::cron::parse(&job.cron);
        let offset_ms = match (&cron, job.next_at_ms) {
            (Some(cron), Some(next)) => cron.jitter_offset_ms(job.created_at_ms, next),
            _ => 0,
        };
        Job {
            job,
            cron,
            offset_ms,
        }
    }

    /// A job from the previous process: its jitter is relearned from the
    /// carried next run, and a run that came due while nothing was running
    /// moves to the next one (Claude does not replay missed runs).
    fn from_carried(job: ScheduledJob) -> Self {
        let mut job = Job::new(job);
        if let (Some(cron), Some(next)) = (&job.cron, job.job.next_at_ms) {
            let now = crate::now_ms();
            if next < now && job.job.recurring {
                job.job.next_at_ms = cron.next_run_ms(now, job.offset_ms);
            }
        }
        job
    }
}

/// Epoch ms from a JSON number (Claude sends `nextFireAt` as a float).
fn as_ms(value: &Value) -> Option<u64> {
    let ms = value.as_f64()?;
    (ms.is_finite() && ms > 0.0).then_some(ms as u64)
}

/// Claude's `/goal`: set and cleared by the command's own replies, checked
/// by the session Stop hook whose "not yet" verdicts arrive as synthetic
/// user frames. Its end (achieved, or could not be achieved) never reaches
/// stdout — only the native transcript's `goal_status` record says which —
/// so the daemon resolves that after the turn (see the server's chat glue).
#[derive(Default)]
pub struct GoalTracker {
    goal: Option<GoalState>,
}

/// What Claude's Stop hook writes back when a check fails:
/// `Stop hook feedback:\n[<condition>]: <reason>` (live 2.1.300).
const STOP_FEEDBACK: &str = "Stop hook feedback:\n[";

impl GoalTracker {
    pub fn seed(&mut self, goal: Option<GoalState>) {
        self.goal = goal.filter(|g| g.status == "active");
    }

    pub fn goal(&self) -> Option<&GoalState> {
        self.goal.as_ref()
    }

    /// The daemon learned how the goal ended (the transcript's verdict):
    /// forget it so a stale goal is never re-emitted.
    pub fn forget(&mut self) {
        self.goal = None;
    }

    /// A `/goal` reply (`local_command_run {command: "goal", args}` on a
    /// synthetic assistant frame). Returns the event and whether the reply's
    /// prose is now said by that event (set and cleared are; a status reply
    /// stays prose — the user asked for it).
    pub fn on_command_reply(
        &mut self,
        args: &str,
        text: &str,
        now_ms: u64,
    ) -> (Option<AgentEvent>, bool) {
        let text = text.trim();
        if text.starts_with("Goal set:") {
            let objective = truncate_label(args.trim(), GOAL_TEXT_MAX);
            if objective.is_empty() {
                return (None, false);
            }
            let goal = GoalState {
                objective,
                status: "active".into(),
                set_at_ms: now_ms,
                turns: Some(0),
                ..Default::default()
            };
            self.goal = Some(goal.clone());
            return (
                Some(AgentEvent::Goal {
                    goal: Some(goal),
                    ended: None,
                }),
                true,
            );
        }
        if let Some(rest) = text.strip_prefix("Goal cleared:") {
            let objective = self
                .goal
                .take()
                .map(|g| g.objective)
                .unwrap_or_else(|| truncate_label(rest.trim(), GOAL_TEXT_MAX));
            return (
                Some(AgentEvent::Goal {
                    goal: None,
                    ended: Some(GoalEnd {
                        outcome: "cleared".into(),
                        objective,
                        ..Default::default()
                    }),
                }),
                true,
            );
        }
        // A status reply is the CLI's own truth: resync without a line. A
        // resumed session keeps its goal (live 2.1.300: "Goal active: <cond>
        // (not yet evaluated)" after `--resume`), so one we never saw set is
        // adopted from its first line.
        if let Some(rest) = text.strip_prefix("Goal active:") {
            if self.goal.is_none() {
                let line = rest.lines().next().unwrap_or_default().trim();
                let objective = line.rfind(" (").map_or(line, |at| &line[..at]).trim();
                if !objective.is_empty() {
                    let goal = GoalState {
                        objective: truncate_label(objective, GOAL_TEXT_MAX),
                        status: "active".into(),
                        set_at_ms: now_ms,
                        ..Default::default()
                    };
                    self.goal = Some(goal.clone());
                    return (
                        Some(AgentEvent::Goal {
                            goal: Some(goal),
                            ended: None,
                        }),
                        false,
                    );
                }
            }
            return (None, false);
        }
        if text.starts_with("No goal set") && self.goal.take().is_some() {
            return (
                Some(AgentEvent::Goal {
                    goal: None,
                    ended: None,
                }),
                false,
            );
        }
        (None, false)
    }

    /// A synthetic user frame's text: a failed check of the active goal
    /// moves its count and reason. Other Stop hooks write the same envelope
    /// with their own command in the brackets, so only the goal's own
    /// condition counts.
    pub fn on_stop_feedback(&mut self, text: &str) -> Option<AgentEvent> {
        let rest = text.strip_prefix(STOP_FEEDBACK)?;
        let goal = self.goal.as_mut()?;
        let reason = rest
            .strip_prefix(goal.objective.as_str())?
            .strip_prefix("]:")?;
        goal.turns = Some(goal.turns.unwrap_or(0).saturating_add(1));
        goal.last_check = Some(truncate_label(reason.trim(), GOAL_REASON_MAX));
        Some(AgentEvent::Goal {
            goal: Some(goal.clone()),
            ended: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const T0: u64 = 1_792_108_736_000;

    fn wakeup_result(at: u64) -> Value {
        json!({"scheduledFor": at, "clampedDelaySeconds": 60, "wasClamped": false})
    }

    #[test]
    fn a_wakeup_is_pending_until_it_fires_then_names_the_turn() {
        let mut t = ScheduleTracker::default();
        t.on_tool_use(
            "tu1",
            "ScheduleWakeup",
            &json!({"delaySeconds": 60, "reason": "Check the deploy", "prompt": "/loop check"}),
        );
        let Some(AgentEvent::Schedule { wakeup, jobs }) =
            t.on_tool_result("tu1", false, Some(&wakeup_result(T0 + 64_000)), T0)
        else {
            panic!("schedule event");
        };
        let wakeup = wakeup.expect("pending wakeup");
        assert_eq!((wakeup.at_ms, wakeup.scheduled_at_ms), (T0 + 64_000, T0));
        assert_eq!(wakeup.reason, "Check the deploy");
        assert!(jobs.is_empty());
        // Too early: a turn the CLI starts a minute before is something else.
        assert!(t.on_cli_turn(T0 + 64_000 - 3 * 60_000).is_none());
        // Live, it fired ~3 s early.
        let (fired, schedule) = t.on_cli_turn(T0 + 61_000).expect("fired");
        assert_eq!(fired.kind, "wakeup");
        assert_eq!(fired.detail, "Check the deploy");
        assert_eq!(
            schedule,
            AgentEvent::Schedule {
                wakeup: None,
                jobs: vec![]
            }
        );
        assert!(t.on_cli_turn(T0 + 62_000).is_none(), "spent");
    }

    #[test]
    fn stop_cancels_the_wakeup_and_a_failed_call_changes_nothing() {
        let mut t = ScheduleTracker::default();
        t.on_tool_use(
            "a",
            "ScheduleWakeup",
            &json!({"delaySeconds": 60, "reason": "r"}),
        );
        assert!(t
            .on_tool_result("a", false, Some(&wakeup_result(T0 + 60_000)), T0)
            .is_some());
        t.on_tool_use("b", "ScheduleWakeup", &json!({"stop": true}));
        assert!(
            t.on_tool_result("b", true, None, T0).is_none(),
            "failed stop"
        );
        assert!(!t.is_empty());
        t.on_tool_use("c", "ScheduleWakeup", &json!({"stop": true}));
        let stopped = json!({"scheduledFor": 0, "stopped": true, "cancelledWakeups": 1});
        assert!(t.on_tool_result("c", false, Some(&stopped), T0).is_some());
        assert!(t.is_empty());
        // Stopping with nothing pending is no change.
        t.on_tool_use("d", "ScheduleWakeup", &json!({"stop": true}));
        assert!(t.on_tool_result("d", false, Some(&stopped), T0).is_none());
    }

    #[test]
    fn a_recurring_job_fires_and_moves_to_its_next_run_with_its_jitter() {
        let mut t = ScheduleTracker::default();
        let created = 1_792_108_859_588;
        t.on_tool_use(
            "c1",
            "CronCreate",
            &json!({"cron": "*/1 * * * *", "prompt": "say tock", "recurring": true}),
        );
        let result = json!({"id": "f2143f41", "humanSchedule": "Every minute",
            "recurring": true, "durable": false, "nextFireAt": 1_792_108_888_368.643_3});
        let Some(AgentEvent::Schedule { jobs, .. }) =
            t.on_tool_result("c1", false, Some(&result), created)
        else {
            panic!("schedule");
        };
        assert_eq!(jobs[0].schedule, "Every minute");
        assert_eq!(jobs[0].next_at_ms, Some(1_792_108_888_368));
        let (fired, AgentEvent::Schedule { jobs, .. }) =
            t.on_cli_turn(1_792_108_888_769).expect("fired")
        else {
            panic!("schedule");
        };
        assert_eq!(fired.kind, "job");
        assert_eq!(fired.detail, "say tock");
        assert_eq!(fired.schedule.as_deref(), Some("Every minute"));
        // The next minute plus the same 28.368 s jitter (live: 948.368).
        assert_eq!(jobs[0].next_at_ms, Some(1_792_108_948_368));
    }

    #[test]
    fn delete_and_list_keep_the_set_level() {
        let mut t = ScheduleTracker::default();
        for (call, id) in [("c1", "aaaa"), ("c2", "bbbb")] {
            t.on_tool_use(
                call,
                "CronCreate",
                &json!({"cron": "0 * * * *", "prompt": id}),
            );
            let r = json!({"id": id, "humanSchedule": "Every hour", "recurring": true, "nextFireAt": T0 + 600_000});
            t.on_tool_result(call, false, Some(&r), T0);
        }
        t.on_tool_use("d", "CronDelete", &json!({"id": "aaaa"}));
        let Some(AgentEvent::Schedule { jobs, .. }) =
            t.on_tool_result("d", false, Some(&json!({"id": "aaaa"})), T0)
        else {
            panic!("schedule");
        };
        assert_eq!(
            jobs.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
            ["bbbb"]
        );
        // CronList is the CLI's whole truth: a job it no longer lists is gone,
        // one it lists that we never saw (made before a restart) is adopted.
        t.on_tool_use("l", "CronList", &json!({}));
        let listed = json!({"jobs": [{"id": "cccc", "cron": "17 3 * * *",
            "humanSchedule": "Every day at 3:17 AM", "prompt": "say nightly",
            "recurring": true, "durable": false, "nextFireAt": T0 + 3_600_000}]});
        let Some(AgentEvent::Schedule { jobs, .. }) =
            t.on_tool_result("l", false, Some(&listed), T0)
        else {
            panic!("schedule");
        };
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].id, "cccc");
        assert_eq!(jobs[0].next_at_ms, Some(T0 + 3_600_000));
    }

    #[test]
    fn one_shot_jobs_leave_after_firing_and_recurring_ones_expire_after_a_week() {
        let mut t = ScheduleTracker::default();
        t.on_tool_use(
            "c",
            "CronCreate",
            &json!({"cron": "30 14 * * *", "prompt": "once", "recurring": false}),
        );
        let r = json!({"id": "one", "humanSchedule": "Today at 2:30 PM", "recurring": false, "nextFireAt": T0 + 1000});
        t.on_tool_result("c", false, Some(&r), T0);
        let (fired, _) = t.on_cli_turn(T0 + 1000).expect("fired");
        assert_eq!(fired.kind, "job");
        assert!(t.is_empty());

        let week = RECURRING_LIFETIME_MS;
        t.seed(
            vec![ScheduledJob {
                id: "old".into(),
                cron: "0 * * * *".into(),
                schedule: "Every hour".into(),
                prompt: "p".into(),
                recurring: true,
                next_at_ms: Some(crate::now_ms() + 60_000),
                created_at_ms: crate::now_ms() - week - 1,
            }],
            crate::now_ms(),
        );
        assert!(t.is_empty(), "past its 7 days");
    }

    #[test]
    fn goal_set_checked_and_cleared_from_the_cli_replies() {
        let mut g = GoalTracker::default();
        let cond = "the file n.txt contains the number 3";
        let (event, said) = g.on_command_reply(cond, &format!("Goal set: {cond}"), T0);
        assert!(said);
        let Some(AgentEvent::Goal {
            goal: Some(goal),
            ended: None,
        }) = event
        else {
            panic!("goal");
        };
        assert_eq!((goal.status.as_str(), goal.turns), ("active", Some(0)));
        // Another Stop hook's feedback is not a goal check.
        assert!(g
            .on_stop_feedback("Stop hook feedback:\n[npm run lint]: 3 errors")
            .is_none());
        let live = format!(
            "Stop hook feedback:\n[{cond}]: The transcript shows n.txt containing 1. \
             Nothing shows it reaching 3, so the condition is not met."
        );
        let Some(AgentEvent::Goal {
            goal: Some(goal), ..
        }) = g.on_stop_feedback(&live)
        else {
            panic!("check");
        };
        assert_eq!(goal.turns, Some(1));
        assert!(goal.last_check.unwrap().starts_with("The transcript shows"));
        let (event, said) = g.on_command_reply("clear", &format!("Goal cleared: {cond}"), T0);
        assert!(said);
        let Some(AgentEvent::Goal {
            goal: None,
            ended: Some(end),
        }) = event
        else {
            panic!("cleared");
        };
        assert_eq!(
            (end.outcome.as_str(), end.objective.as_str()),
            ("cleared", cond)
        );
        assert!(g.goal().is_none());
    }

    #[test]
    fn a_status_reply_resyncs_but_stays_prose() {
        let mut g = GoalTracker::default();
        g.seed(Some(GoalState {
            objective: "x".into(),
            status: "active".into(),
            ..Default::default()
        }));
        let (event, said) = g.on_command_reply("", "No goal set. Usage: `/goal <condition>`", T0);
        assert!(!said);
        assert_eq!(
            event,
            Some(AgentEvent::Goal {
                goal: None,
                ended: None
            })
        );
        let (event, _) = g.on_command_reply("", "No goal set. Usage: `/goal <condition>`", T0);
        assert!(event.is_none(), "already none");
        // Live after `--resume`: the goal we never saw set is adopted.
        let (event, said) = g.on_command_reply(
            "",
            "Goal active: the file zz.txt contains the word banana (not yet evaluated)",
            T0,
        );
        assert!(!said);
        let Some(AgentEvent::Goal {
            goal: Some(goal), ..
        }) = event
        else {
            panic!("adopted");
        };
        assert_eq!(goal.objective, "the file zz.txt contains the word banana");
    }
}
