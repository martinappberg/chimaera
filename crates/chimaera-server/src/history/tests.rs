//! Pure pieces of the session history: the running-total accumulator, the
//! line format, merge, compaction into month totals, and the usage math.
//! The lifecycle against a real `AppState` lives in `src/tests/history.rs`.

use super::usage::{self, Input, Row, Summary};
use super::*;

fn rec(rid: &str, started: u64, agent: &str, model: &str, cost: Option<f64>) -> Record {
    Record {
        rid: rid.into(),
        id: rid.split('@').next().unwrap_or(rid).into(),
        agent: agent.into(),
        ui: "chat".into(),
        title: Some(format!("session {rid}")),
        models: vec![model.into()],
        started_by: "you".into(),
        started,
        ended: Some(started + 60_000),
        outcome: Some(Outcome::Exited),
        usage: Usage {
            cost_usd: cost,
            tokens_in: Some(100),
            tokens_out: Some(10),
            turns: Some(1),
        },
        ..Record::default()
    }
}

fn temp(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "chimaera-history-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn running_totals_count_what_this_record_spent() {
    // A fresh TUI: the statusline paints 0 before the first prompt.
    let mut r = Running::default();
    r.see(0.0, true, None);
    r.see(0.5, false, None);
    r.see(1.25, false, None);
    assert_eq!(r.value(), Some(1.25));

    // A resumed TUI whose counter claude restored: the pre-turn value is
    // the baseline, never this record's spend.
    let mut r = Running::default();
    r.see(4.0, true, Some(4.0));
    r.see(4.3, false, Some(4.0));
    assert!((r.value().unwrap() - 0.3).abs() < 1e-9);

    // A chat continuing a conversation whose total carried over: the
    // previous record's total is subtracted.
    let mut r = Running::default();
    r.see(5.2, false, Some(5.0));
    assert!((r.value().unwrap() - 0.2).abs() < 1e-9);

    // ...unless the counter evidently restarted (below the prior).
    let mut r = Running::default();
    r.see(0.4, false, Some(5.0));
    assert!((r.value().unwrap() - 0.4).abs() < 1e-9);

    // A view switch respawned the process without restoring: the drop
    // counts the new value whole.
    let mut r = Running::default();
    r.see(1.0, false, None);
    r.see(0.3, false, None);
    assert!((r.value().unwrap() - 1.3).abs() < 1e-9);

    // Nothing seen: unknown, never zero; hostile values are ignored.
    let mut r = Running::default();
    assert_eq!(r.value(), None);
    r.see(f64::NAN, false, None);
    r.see(-1.0, false, None);
    assert_eq!(r.value(), None);
}

#[test]
fn started_by_wire_forms() {
    assert_eq!(StartedBy::You.wire(), "you");
    assert_eq!(StartedBy::Mastermind.wire(), "mastermind");
    assert_eq!(StartedBy::Restart.wire(), "restart");
    assert_eq!(StartedBy::Session("s-abc".into()).wire(), "s-abc");
}

#[test]
fn lines_put_the_tag_first_and_keep_unknown_kinds() {
    let line = serde_json::to_string(&Line::Open(rec("s-1@1", 1, "claude", "opus", None))).unwrap();
    assert!(line.starts_with("{\"t\":\"open\""), "{line}");
    // Unknown usage is null on the wire, never zero.
    assert!(line.contains("\"cost_usd\":null"), "{line}");
    let parsed: Line = serde_json::from_str(r#"{"t":"hologram","x":1}"#).unwrap();
    assert!(matches!(parsed, Line::Unknown));
    let act: Line = serde_json::from_str(
        r#"{"t":"act","ts":5,"by":"s-mm","act":"spawn_agent","target":"s-2"}"#,
    )
    .unwrap();
    assert!(matches!(act, Line::Act(ref a) if a.target.as_deref() == Some("s-2")));
}

#[test]
fn merge_lets_the_close_line_win() {
    let mut open = rec("s-1@1", 1, "claude", "opus", None);
    open.ended = None;
    open.outcome = None;
    open.usage = Usage::default();
    let close = rec("s-1@1", 1, "claude", "opus", Some(0.5));
    let other = rec("s-2@2", 2, "codex", "gpt-5", None);
    let parsed = merge(vec![
        Line::Open(open),
        Line::Open(other.clone()),
        Line::Close(close.clone()),
        Line::Act(Act {
            ts: 3,
            by: "you".into(),
            act: "deliver_note".into(),
            target: None,
            detail: None,
        }),
    ]);
    assert_eq!(parsed.records, vec![close, other]);
    assert_eq!(parsed.acts.len(), 1);
}

#[test]
fn compaction_folds_old_records_into_month_totals() {
    let dir = temp("compact");
    let path = dir.join("sessions.jsonl");
    let mut body = String::new();
    // 2026-07-01 and 2026-08-01, UTC.
    let july = 1_782_864_000_000u64;
    let august = july + 31 * 86_400_000;
    let mut n = 0u64;
    for (base, count) in [(july, 40u64), (august, 40u64)] {
        for i in 0..count {
            n += 1;
            let r = rec(
                &format!("s-{n}@{}", base + i),
                base + i * 1000,
                "claude",
                "opus",
                Some(0.25),
            );
            body.push_str(&serde_json::to_string(&Line::Open(r.clone())).unwrap());
            body.push('\n');
            body.push_str(&serde_json::to_string(&Line::Close(r)).unwrap());
            body.push('\n');
        }
    }
    // One still-open record from July: it is live, so it always stays.
    let mut open = rec("s-live@9", july + 5, "codex", "gpt-5", None);
    open.ended = None;
    body.push_str(&serde_json::to_string(&Line::Open(open)).unwrap());
    body.push('\n');
    std::fs::write(&path, &body).unwrap();

    // Keep only a few records' worth of detail.
    compact(&path, 6 * 1024).unwrap();
    let parsed = merge(read_lines(&path));
    let kept = parsed.records.len();
    assert!(kept > 1 && kept < 81, "kept {kept}");
    assert!(
        parsed.records.iter().any(|r| r.rid == "s-live@9"),
        "an open record is never folded"
    );
    // Everything dropped is in the month totals: counts and cost add up.
    let folded: u64 = parsed.months.iter().map(|m| m.sessions).sum();
    assert_eq!(folded + (kept as u64 - 1), 80);
    let cost: f64 = parsed
        .months
        .iter()
        .flat_map(|m| m.rows.iter())
        .map(|r| r.cost_usd)
        .sum();
    assert!((cost - folded as f64 * 0.25).abs() < 1e-6);
    assert!(parsed.months.iter().all(|m| m.month.starts_with("2026-0")));
    // The newest records are the ones kept.
    assert!(parsed
        .records
        .iter()
        .any(|r| r.rid == format!("s-80@{}", august + 39)));

    // Compacting again folds into the existing month lines, not beside them.
    compact(&path, 2 * 1024).unwrap();
    let again = merge(read_lines(&path));
    let folded_again: u64 = again.months.iter().map(|m| m.sessions).sum();
    assert_eq!(folded_again + (again.records.len() as u64 - 1), 80);
    let raw = std::fs::read_to_string(&path).unwrap();
    let month_lines = raw
        .lines()
        .filter(|l| l.starts_with("{\"t\":\"month\""))
        .count();
    assert_eq!(month_lines, again.months.len());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn usage_report_buckets_days_weeks_and_models_honestly() {
    // 2026-09-28 12:00 UTC is a Monday.
    let now = 1_790_596_800_000u64;
    let day = 86_400_000u64;
    let rows = vec![
        Row {
            started: now - 1000,
            agent: "claude".into(),
            model: Some("opus".into()),
            cost: Some(1.5),
            tin: Some(1000),
            tout: Some(100),
            duration_ms: 60_000,
        },
        Row {
            started: now - day,
            agent: "codex".into(),
            model: Some("gpt-5".into()),
            cost: None,
            tin: Some(500),
            tout: Some(50),
            duration_ms: 60_000,
        },
        Row {
            started: now - 10 * day,
            agent: "claude".into(),
            model: Some("opus".into()),
            cost: Some(0.5),
            tin: None,
            tout: None,
            duration_ms: 60_000,
        },
    ];
    let months = vec![Month {
        month: "2026-06".into(),
        sessions: 3,
        rows: vec![MonthRow {
            agent: "claude".into(),
            model: Some("opus".into()),
            sessions: 3,
            cost_usd: 2.0,
            cost_sessions: 2,
            tokens_in: 10,
            tokens_out: 1,
            token_sessions: 3,
            duration_ms: 180_000,
        }],
    }];
    let inputs = vec![Input {
        ws: "w1".into(),
        name: "proj".into(),
        summary: Arc::new(Summary { rows, months }),
        live: vec![],
    }];
    let r = usage::report(&inputs, now, 0, 14, 4);
    assert_eq!(r.basis, "estimated at API prices");
    assert_eq!(r.totals.sessions, 6);
    // Time agents spent working: every session's duration, folded ones too.
    assert_eq!(r.totals.duration_ms, 3 * 60_000 + 180_000);
    assert!((r.totals.cost_usd - 4.0).abs() < 1e-9);
    // The codex session and one folded one have no cost: unknown, not $0.
    assert_eq!(r.totals.unknown_cost_sessions, 2);
    assert_eq!(r.today.sessions, 1);
    assert_eq!(r.week.sessions, 2);
    assert_eq!(r.days.len(), 14);
    assert_eq!(r.days.last().unwrap().key.day, "2026-09-28");
    assert_eq!(r.days.last().unwrap().agg.sessions, 1);
    assert_eq!(r.weeks.len(), 4);
    assert_eq!(r.weeks.last().unwrap().key.week, "2026-09-28");
    let opus = r
        .by_agent_model
        .iter()
        .find(|k| k.key.model.as_deref() == Some("opus"))
        .unwrap();
    assert_eq!(opus.agg.sessions, 5);
    assert!(r
        .months
        .iter()
        .any(|m| m.key.month == "2026-06" && m.key.folded));
    assert_eq!(r.workspaces[0].key.name, "proj");

    // A timezone moves the day boundary: 00:30 local on the 29th at +13h.
    let r = usage::report(&inputs, now, 13 * 60, 3, 1);
    assert_eq!(r.days.last().unwrap().key.day, "2026-09-29");
}

#[test]
fn csv_quotes_formula_cells_and_leaves_unknown_empty() {
    let mut r = rec(
        "s-1@1790596800000",
        1_790_596_800_000,
        "codex",
        "gpt-5",
        None,
    );
    r.title = Some("=HYPERLINK(\"x\")".into());
    let out = usage::csv(&[("proj".into(), vec![r], vec![])], 0);
    let mut lines = out.lines();
    assert!(lines.next().unwrap().starts_with("workspace,started,"));
    let row = lines.next().unwrap();
    assert!(row.contains("'=HYPERLINK"), "{row}");
    assert!(row.contains("2026-09-28 12:00"), "{row}");
    // Unknown cost is an empty cell, never 0.
    assert!(row.contains(",100,10,,"), "{row}");
}
