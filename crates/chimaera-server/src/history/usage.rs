//! Activity totals from the session records (plan §10): sessions, tokens
//! and time agents spent working — per workspace, per day and week, per
//! agent and model — and a CSV export. The UI leads with sessions and tokens
//! and shows no dollars; cost (what the agent reports at API prices) is
//! still summed here and exported as `estimated_cost_usd`. A session whose
//! agent reports nothing counts as unknown — never as zero.
//!
//! Each workspace's file parses once per change into compact rows, cached
//! (`Cache`, at most `CACHE_MAX` workspaces) and keyed by the file's length
//! and mtime; the open records add their usage so far at query time.

use std::collections::{BTreeMap, VecDeque};
use std::path::Path;
use std::sync::Arc;

use serde::Serialize;

use super::{merge, primary_model, read_lines, Month, Record};

const CACHE_MAX: usize = 16;
pub(crate) const DAYS_DEFAULT: u32 = 14;
pub(crate) const DAYS_MAX: u32 = 400;
pub(crate) const WEEKS_DEFAULT: u32 = 8;
pub(crate) const WEEKS_MAX: u32 = 104;
/// Minutes east of UTC accepted for day/week boundaries (UTC−14..UTC+14).
const TZ_MAX_MIN: i64 = 14 * 60;
const DAY_MS: i64 = 86_400_000;

/// What aggregation needs of one record.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Row {
    pub(crate) started: u64,
    pub(crate) agent: String,
    pub(crate) model: Option<String>,
    pub(crate) cost: Option<f64>,
    pub(crate) tin: Option<u64>,
    pub(crate) tout: Option<u64>,
    /// How long it ran (so far, for an open record), ms.
    pub(crate) duration_ms: u64,
}

impl Row {
    pub(crate) fn of(rec: &Record) -> Row {
        Row {
            started: rec.started,
            agent: rec.agent.clone(),
            model: primary_model(rec),
            cost: rec.usage.cost_usd,
            tin: rec.usage.tokens_in,
            tout: rec.usage.tokens_out,
            duration_ms: rec
                .ended
                .unwrap_or_else(super::now_ms)
                .saturating_sub(rec.started),
        }
    }
}

/// One workspace's file, summarized.
#[derive(Default, Debug)]
pub(crate) struct Summary {
    pub(crate) rows: Vec<Row>,
    pub(crate) months: Vec<Month>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileKey {
    len: u64,
    mtime_ns: u128,
}

fn file_key(path: &Path) -> Option<FileKey> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime_ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(FileKey {
        len: meta.len(),
        mtime_ns,
    })
}

#[derive(Default)]
pub(crate) struct Cache {
    entries: VecDeque<(String, FileKey, Arc<Summary>)>,
}

impl Cache {
    pub(crate) fn forget(&mut self, ws: &str) {
        self.entries.retain(|(w, _, _)| w != ws);
    }
}

/// A workspace's summary from the cache, or parsed afresh. BLOCKING (stats
/// and maybe reads the file): call off the reactor.
pub(crate) fn summary(cache: &std::sync::Mutex<Cache>, ws: &str, path: &Path) -> Arc<Summary> {
    let Some(key) = file_key(path) else {
        return Arc::new(Summary::default());
    };
    if let Some((_, _, s)) = crate::lock(cache)
        .entries
        .iter()
        .find(|(w, k, _)| w == ws && *k == key)
    {
        return s.clone();
    }
    let parsed = merge(read_lines(path));
    let summary = Arc::new(Summary {
        // Open records in the file are counted from memory instead (their
        // usage so far); a record left open by a crash has no usage anyway.
        rows: parsed
            .records
            .iter()
            .filter(|r| r.ended.is_some())
            .map(Row::of)
            .collect(),
        months: parsed.months,
    });
    let mut cache = crate::lock(cache);
    cache.forget(ws);
    cache
        .entries
        .push_back((ws.to_string(), key, summary.clone()));
    while cache.entries.len() > CACHE_MAX {
        cache.entries.pop_front();
    }
    summary
}

/// A total: `cost_usd` sums the sessions whose cost is known; the rest are
/// counted in `unknown_cost_sessions`, never as zero.
#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub(crate) struct Agg {
    pub(crate) sessions: u64,
    pub(crate) cost_usd: f64,
    pub(crate) cost_sessions: u64,
    pub(crate) unknown_cost_sessions: u64,
    pub(crate) tokens_in: u64,
    pub(crate) tokens_out: u64,
    pub(crate) token_sessions: u64,
    /// Time agents spent working: the sum of the sessions' durations, ms.
    pub(crate) duration_ms: u64,
}

impl Agg {
    fn add(&mut self, row: &Row) {
        self.sessions += 1;
        self.duration_ms += row.duration_ms;
        match row.cost {
            Some(c) => {
                self.cost_usd += c;
                self.cost_sessions += 1;
            }
            None => self.unknown_cost_sessions += 1,
        }
        if row.tin.is_some() || row.tout.is_some() {
            self.tokens_in += row.tin.unwrap_or(0);
            self.tokens_out += row.tout.unwrap_or(0);
            self.token_sessions += 1;
        }
    }

    fn add_month_row(&mut self, r: &super::MonthRow) {
        self.sessions += r.sessions;
        self.duration_ms += r.duration_ms;
        self.cost_usd += r.cost_usd;
        self.cost_sessions += r.cost_sessions;
        self.unknown_cost_sessions += r.sessions.saturating_sub(r.cost_sessions);
        self.tokens_in += r.tokens_in;
        self.tokens_out += r.tokens_out;
        self.token_sessions += r.token_sessions;
    }

    fn rounded(mut self) -> Self {
        self.cost_usd = (self.cost_usd * 100.0).round() / 100.0;
        self
    }
}

/// The aggregation's inputs: per workspace, its closed rows, its folded
/// months, and its open records' usage so far.
pub(crate) struct Input {
    pub(crate) ws: String,
    pub(crate) name: String,
    pub(crate) summary: Arc<Summary>,
    pub(crate) live: Vec<Row>,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Keyed<K: Serialize> {
    #[serde(flatten)]
    pub(crate) key: K,
    #[serde(flatten)]
    pub(crate) agg: Agg,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct WsKey {
    pub(crate) id: String,
    pub(crate) name: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct ModelKey {
    pub(crate) agent: String,
    pub(crate) model: Option<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct DayKey {
    pub(crate) day: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct WeekKey {
    /// The week's Monday (local date).
    pub(crate) week: String,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct MonthKey {
    pub(crate) month: String,
    /// Some of the month's sessions were compacted into totals (no per-day
    /// detail, and counted by UTC month).
    pub(crate) folded: bool,
}

#[derive(Serialize, Debug, PartialEq)]
pub(crate) struct Report {
    pub(crate) schema: u32,
    /// What the cost means — the page says it verbatim.
    pub(crate) basis: &'static str,
    pub(crate) now: u64,
    pub(crate) tz: i64,
    pub(crate) totals: Agg,
    pub(crate) today: Agg,
    /// The last seven days, today included.
    pub(crate) week: Agg,
    pub(crate) workspaces: Vec<Keyed<WsKey>>,
    pub(crate) by_agent_model: Vec<Keyed<ModelKey>>,
    pub(crate) days: Vec<Keyed<DayKey>>,
    pub(crate) weeks: Vec<Keyed<WeekKey>>,
    pub(crate) months: Vec<Keyed<MonthKey>>,
    /// The oldest detail record (ms), when any.
    pub(crate) since: Option<u64>,
}

pub(crate) const BASIS: &str = "estimated at API prices";

pub(crate) fn clamp_tz(tz: Option<i64>) -> i64 {
    tz.unwrap_or(0).clamp(-TZ_MAX_MIN, TZ_MAX_MIN)
}

/// Local day number (days since 1970-01-01 at `tz` minutes east of UTC).
fn day_of(ms: u64, tz: i64) -> i64 {
    (ms as i64 + tz * 60_000).div_euclid(DAY_MS)
}

fn day_label(day: i64) -> String {
    let (y, m, d) = crate::download::civil_from_days(day);
    format!("{y:04}-{m:02}-{d:02}")
}

/// `YYYY-MM` of a timestamp at `tz` minutes east of UTC.
pub(crate) fn month_of(ms: u64, tz: i64) -> String {
    let (y, m, _) = crate::download::civil_from_days(day_of(ms, tz));
    format!("{y:04}-{m:02}")
}

/// The Monday starting the week of `day` (1970-01-01 was a Thursday).
fn week_start(day: i64) -> i64 {
    day - (day + 3).rem_euclid(7)
}

pub(crate) fn report(inputs: &[Input], now: u64, tz: i64, days: u32, weeks: u32) -> Report {
    let today = day_of(now, tz);
    let days = days.clamp(1, DAYS_MAX) as i64;
    let weeks = weeks.clamp(1, WEEKS_MAX) as i64;
    let first_day = today - (days - 1);
    let this_week = week_start(today);
    let first_week = this_week - (weeks - 1) * 7;

    let mut totals = Agg::default();
    let mut today_agg = Agg::default();
    let mut week_agg = Agg::default();
    let mut by_ws: Vec<Keyed<WsKey>> = Vec::new();
    let mut by_model: BTreeMap<(String, Option<String>), Agg> = BTreeMap::new();
    let mut by_day: BTreeMap<i64, Agg> = BTreeMap::new();
    let mut by_week: BTreeMap<i64, Agg> = BTreeMap::new();
    let mut by_month: BTreeMap<String, (Agg, bool)> = BTreeMap::new();
    let mut since: Option<u64> = None;

    for input in inputs {
        let mut ws_agg = Agg::default();
        for row in input.summary.rows.iter().chain(input.live.iter()) {
            let day = day_of(row.started, tz);
            ws_agg.add(row);
            totals.add(row);
            by_model
                .entry((row.agent.clone(), row.model.clone()))
                .or_default()
                .add(row);
            if day == today {
                today_agg.add(row);
            }
            if day > today - 7 && day <= today {
                week_agg.add(row);
            }
            if day >= first_day && day <= today {
                by_day.entry(day).or_default().add(row);
            }
            let week = week_start(day);
            if week >= first_week && week <= this_week {
                by_week.entry(week).or_default().add(row);
            }
            by_month
                .entry(month_of(row.started, tz))
                .or_default()
                .0
                .add(row);
            since = Some(since.map_or(row.started, |s| s.min(row.started)));
        }
        for month in &input.summary.months {
            let slot = by_month.entry(month.month.clone()).or_default();
            slot.1 = true;
            for r in &month.rows {
                slot.0.add_month_row(r);
                ws_agg.add_month_row(r);
                totals.add_month_row(r);
                by_model
                    .entry((r.agent.clone(), r.model.clone()))
                    .or_default()
                    .add_month_row(r);
            }
        }
        by_ws.push(Keyed {
            key: WsKey {
                id: input.ws.clone(),
                name: input.name.clone(),
            },
            agg: ws_agg.rounded(),
        });
    }
    by_ws.sort_by(|a, b| {
        b.agg
            .cost_usd
            .partial_cmp(&a.agg.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.agg.sessions.cmp(&a.agg.sessions))
    });
    let mut by_agent_model: Vec<Keyed<ModelKey>> = by_model
        .into_iter()
        .map(|((agent, model), agg)| Keyed {
            key: ModelKey { agent, model },
            agg: agg.rounded(),
        })
        .collect();
    by_agent_model.sort_by(|a, b| {
        b.agg
            .cost_usd
            .partial_cmp(&a.agg.cost_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.agg.sessions.cmp(&a.agg.sessions))
    });
    // Every day and week in the window, empty ones included (a chart needs
    // its zeros — sessions: 0 is a fact, not an unknown).
    let days_out = (first_day..=today)
        .map(|d| Keyed {
            key: DayKey { day: day_label(d) },
            agg: by_day.remove(&d).unwrap_or_default().rounded(),
        })
        .collect();
    let weeks_out = (0..weeks)
        .map(|i| first_week + i * 7)
        .map(|w| Keyed {
            key: WeekKey { week: day_label(w) },
            agg: by_week.remove(&w).unwrap_or_default().rounded(),
        })
        .collect();
    let months_out = by_month
        .into_iter()
        .rev()
        .map(|(month, (agg, folded))| Keyed {
            key: MonthKey { month, folded },
            agg: agg.rounded(),
        })
        .collect();
    Report {
        schema: 1,
        basis: BASIS,
        now,
        tz,
        totals: totals.rounded(),
        today: today_agg.rounded(),
        week: week_agg.rounded(),
        workspaces: by_ws,
        by_agent_model,
        days: days_out,
        weeks: weeks_out,
        months: months_out,
        since,
    }
}

/// A spreadsheet can execute a cell that starts like a formula; agent and
/// user text lands in these cells, so such a cell is quoted as text.
fn cell(text: &str) -> String {
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{flat}")
    } else {
        flat
    }
}

fn local_time(ms: u64, tz: i64) -> String {
    let local = ms as i64 + tz * 60_000;
    let day = local.div_euclid(DAY_MS);
    let rem = local.rem_euclid(DAY_MS) / 1000;
    format!(
        "{} {:02}:{:02}",
        day_label(day),
        rem / 3600,
        rem % 3600 / 60
    )
}

/// The CSV export: one row per recorded session (newest first), then one per
/// folded month's agent and model. Unknown values are empty cells.
pub(crate) fn csv(workspaces: &[(String, Vec<Record>, Vec<Month>)], tz: i64) -> String {
    let mut w = ::csv::Writer::from_writer(Vec::new());
    let _ = w.write_record([
        "workspace",
        "started",
        "ended",
        "duration_s",
        "agent",
        "model",
        "started_by",
        "outcome",
        "turns",
        "tokens_in",
        "tokens_out",
        "estimated_cost_usd",
        "title",
    ]);
    let opt = |v: Option<String>| v.unwrap_or_default();
    for (name, records, months) in workspaces {
        for rec in records {
            let duration = rec
                .ended
                .map(|e| (e.saturating_sub(rec.started) / 1000).to_string());
            let _ = w.write_record([
                cell(name),
                local_time(rec.started, tz),
                opt(rec.ended.map(|e| local_time(e, tz))),
                opt(duration),
                cell(&rec.agent),
                cell(&opt(primary_model(rec))),
                cell(&rec.started_by),
                opt(rec.outcome.map(|o| format!("{o:?}").to_lowercase())),
                opt(rec.usage.turns.map(|t| t.to_string())),
                opt(rec.usage.tokens_in.map(|t| t.to_string())),
                opt(rec.usage.tokens_out.map(|t| t.to_string())),
                opt(rec.usage.cost_usd.map(|c| format!("{c:.4}"))),
                cell(rec.title.as_deref().unwrap_or("")),
            ]);
        }
        for month in months {
            for r in &month.rows {
                let _ = w.write_record([
                    cell(name),
                    month.month.clone(),
                    String::new(),
                    (r.duration_ms / 1000).to_string(),
                    cell(&r.agent),
                    cell(r.model.as_deref().unwrap_or("")),
                    String::new(),
                    String::new(),
                    String::new(),
                    if r.token_sessions > 0 {
                        r.tokens_in.to_string()
                    } else {
                        String::new()
                    },
                    if r.token_sessions > 0 {
                        r.tokens_out.to_string()
                    } else {
                        String::new()
                    },
                    if r.cost_sessions > 0 {
                        format!("{:.4}", r.cost_usd)
                    } else {
                        String::new()
                    },
                    format!("{} earlier sessions (monthly totals)", r.sessions),
                ]);
            }
        }
    }
    String::from_utf8(w.into_inner().unwrap_or_default()).unwrap_or_default()
}
