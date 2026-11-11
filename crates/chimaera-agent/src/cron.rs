//! Run times of the 5-field cron schedules agents create.
//!
//! Claude Code's `/loop` registers a session job through `CronCreate` and
//! reports one `nextFireAt`; once the job fires the wire says nothing more,
//! so the driver computes later runs itself to keep a countdown honest. An
//! expression is read in the local time zone of the host the agent runs on,
//! which is the daemon's own host.

/// Longest expression accepted; a schedule is one short line.
pub const CRON_EXPR_MAX: usize = 256;
/// How far a search looks before calling a schedule impossible
/// (`0 0 30 2 *`).
const SEARCH_HORIZON_SECS: i64 = 2 * 366 * 86_400;
/// Candidates a search reads at most, whatever the converter answers.
const SEARCH_STEPS_MAX: u32 = 50_000;
/// Slots walked to place a first fire; jitter is a fraction of one period.
const JITTER_SLOTS_MAX: u32 = 64;

const MONTHS: [&str; 12] = [
    "JAN", "FEB", "MAR", "APR", "MAY", "JUN", "JUL", "AUG", "SEP", "OCT", "NOV", "DEC",
];
const WEEKDAYS: [&str; 7] = ["SUN", "MON", "TUE", "WED", "THU", "FRI", "SAT"];

/// A parsed schedule: one bitset per field, indexed by the field's value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cron {
    minutes: u64,
    hours: u32,
    /// Bits 1-31.
    days: u32,
    /// Bits 1-12.
    months: u16,
    /// Bits 0-6, Sunday = 0 (7 folds onto it).
    weekdays: u8,
    /// The day-of-month / day-of-week field starts with `*` (vixie cron's
    /// test, so `*/2` counts): when neither does, a day matches if EITHER
    /// matches; otherwise both must.
    dom_star: bool,
    dow_star: bool,
}

/// A local wall-clock minute: the fields a schedule matches against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTm {
    pub year: i32,
    /// 1-12.
    pub month: u8,
    /// 1-31.
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    /// 0 = Sunday.
    pub weekday: u8,
}

impl LocalTm {
    fn valid(&self) -> bool {
        (1..=12).contains(&self.month)
            && (1..=31).contains(&self.day)
            && self.hour < 24
            && self.minute < 60
            && self.weekday < 7
    }

    /// Minutes since the epoch as if this wall clock were UTC; the
    /// difference between two readings is how far the wall clock moved.
    fn wall_minutes(&self) -> i64 {
        let days = days_from_civil(
            i64::from(self.year),
            i64::from(self.month),
            i64::from(self.day),
        );
        days * 1440 + i64::from(self.hour) * 60 + i64::from(self.minute)
    }
}

/// Parse a standard 5-field expression (`minute hour dom month dow`). Each
/// field is a comma list of `*`, `a`, `a-b`, each optionally `/step` (`a/n`
/// means `a-max/n`); months and weekdays also take their three-letter
/// English names. Anything else, including `@hourly`-style macros, is `None`.
pub fn parse(expr: &str) -> Option<Cron> {
    if expr.len() > CRON_EXPR_MAX {
        return None;
    }
    let mut fields = expr.split_whitespace();
    let [minute, hour, dom, month, dow] = [
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
        fields.next()?,
    ];
    if fields.next().is_some() {
        return None;
    }
    let weekdays = field(dow, 0, 7, &WEEKDAYS)?;
    Some(Cron {
        minutes: field(minute, 0, 59, &[])?,
        hours: u32::try_from(field(hour, 0, 23, &[])?).ok()?,
        days: u32::try_from(field(dom, 1, 31, &[])?).ok()?,
        months: u16::try_from(field(month, 1, 12, &MONTHS)?).ok()?,
        weekdays: u8::try_from((weekdays | weekdays >> 7) & 0x7f).ok()?,
        dom_star: dom.starts_with('*'),
        dow_star: dow.starts_with('*'),
    })
}

/// One field as a bitset over `lo..=hi`; `names[i]` stands for `lo + i`.
fn field(spec: &str, lo: u32, hi: u32, names: &[&str]) -> Option<u64> {
    let mut bits = 0u64;
    for item in spec.split(',') {
        let (range, step) = match item.split_once('/') {
            Some((range, step)) => (range, Some(number(step)?)),
            None => (item, None),
        };
        let (start, end) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a, lo, names)?, value(b, lo, names)?)
        } else {
            let a = value(range, lo, names)?;
            (a, if step.is_some() { hi } else { a })
        };
        let step = step.unwrap_or(1);
        if step == 0 || start < lo || end > hi || start > end {
            return None;
        }
        for v in (start..=end).step_by(usize::try_from(step).ok()?) {
            bits |= 1 << v;
        }
    }
    Some(bits)
}

fn value(s: &str, lo: u32, names: &[&str]) -> Option<u32> {
    number(s).or_else(|| {
        let i = names.iter().position(|n| n.eq_ignore_ascii_case(s))?;
        lo.checked_add(u32::try_from(i).ok()?)
    })
}

/// Plain digits only (`str::parse` would also take a leading `+`).
fn number(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn bit(mask: u64, i: u8) -> bool {
    i < 64 && mask >> i & 1 == 1
}

impl Cron {
    fn day_matches(&self, tm: &LocalTm) -> bool {
        if !bit(self.months.into(), tm.month) {
            return false;
        }
        let dom = bit(self.days.into(), tm.day);
        let dow = bit(self.weekdays.into(), tm.weekday);
        if self.dom_star || self.dow_star {
            dom && dow
        } else {
            dom || dow
        }
    }

    fn matches(&self, tm: &LocalTm) -> bool {
        self.day_matches(tm) && bit(self.hours.into(), tm.hour) && bit(self.minutes, tm.minute)
    }

    /// Wall-clock minutes from a non-matching `tm` to the next one that
    /// could match: the next local midnight, the next hour, or the next
    /// listed minute of this hour.
    fn skip(&self, tm: &LocalTm) -> i64 {
        let minute = i64::from(tm.minute);
        if !self.day_matches(tm) {
            return 1440 - i64::from(tm.hour) * 60 - minute;
        }
        if !bit(self.hours.into(), tm.hour) {
            return 60 - minute;
        }
        match self.minutes >> (tm.minute + 1) {
            0 => 60 - minute,
            later => i64::from(later.trailing_zeros()) + 1,
        }
    }

    /// The first local minute boundary strictly after `after_ms` that
    /// matches, in epoch ms, reading wall clocks through `tm` (epoch secs to
    /// local fields). Time advances on the real clock and every candidate is
    /// re-read, so a skipped DST hour never matches and a repeated one may
    /// match twice. `None` past a two-year search.
    pub fn next_after_with(
        &self,
        after_ms: u64,
        tm: impl Fn(i64) -> Option<LocalTm>,
    ) -> Option<u64> {
        let mut t = i64::try_from(after_ms / 60_000 + 1).ok()?.checked_mul(60)?;
        let horizon = t.saturating_add(SEARCH_HORIZON_SECS);
        // A jump is sized on the wall clock. When the wall clock moved
        // further than the jump, a spring-forward inside it may have carried
        // it past minutes that exist; those are walked one at a time.
        let mut careful_until = t;
        let mut jumped: Option<(i64, i64, i64)> = None;
        for _ in 0..SEARCH_STEPS_MAX {
            if t > horizon {
                return None;
            }
            let local = tm(t).filter(LocalTm::valid)?;
            let wall = local.wall_minutes();
            if let Some((from, from_wall, minutes)) = jumped.take() {
                if wall.saturating_sub(from_wall) > minutes {
                    careful_until = t;
                    t = from.checked_add(60)?;
                    continue;
                }
            }
            if self.matches(&local) {
                return u64::try_from(t).ok()?.checked_mul(1000);
            }
            let minutes = if t < careful_until {
                1
            } else {
                self.skip(&local)
            };
            if minutes > 1 {
                jumped = Some((t, wall, minutes));
            }
            t = t.checked_add(minutes.checked_mul(60)?)?;
        }
        None
    }

    /// [`Self::next_after_with`] on the host's local clock.
    pub fn next_after(&self, after_ms: u64) -> Option<u64> {
        self.next_after_with(after_ms, local_tm)
    }

    /// How late the agent runs this job behind its exact slots, from the
    /// `nextFireAt` it reported at creation: `next_fire_ms` minus the latest
    /// slot at or before it that is still after `created_at_ms`. Claude
    /// delays every run of a recurring job by one stable per-job offset, so
    /// this one reading reproduces its later runs ([`Self::next_run_ms`]).
    /// 0 when no slot fits (an early fire, or none within a few periods).
    pub fn jitter_offset_ms(&self, created_at_ms: u64, next_fire_ms: u64) -> u64 {
        self.jitter_offset_with(created_at_ms, next_fire_ms, local_tm)
    }

    fn jitter_offset_with(
        &self,
        created_at_ms: u64,
        next_fire_ms: u64,
        tm: impl Fn(i64) -> Option<LocalTm>,
    ) -> u64 {
        let mut latest = None;
        let mut after = created_at_ms;
        for _ in 0..JITTER_SLOTS_MAX {
            match self.next_after_with(after, &tm) {
                Some(slot) if slot <= next_fire_ms => {
                    latest = Some(slot);
                    after = slot;
                }
                _ => return latest.map_or(0, |slot| next_fire_ms.saturating_sub(slot)),
            }
        }
        0
    }

    /// The run after one that fired at `fired_at_ms`, for a job late by
    /// `offset_ms` ([`Self::jitter_offset_ms`]): the next exact slot after
    /// the fire's own slot, plus the same offset.
    pub fn next_run_ms(&self, fired_at_ms: u64, offset_ms: u64) -> Option<u64> {
        self.next_run_with(fired_at_ms, offset_ms, local_tm)
    }

    fn next_run_with(
        &self,
        fired_at_ms: u64,
        offset_ms: u64,
        tm: impl Fn(i64) -> Option<LocalTm>,
    ) -> Option<u64> {
        self.next_after_with(fired_at_ms.saturating_sub(offset_ms), tm)
            .map(|slot| slot.saturating_add(offset_ms))
    }
}

/// The host's local wall clock at `epoch_secs`, by the C library's zone
/// rules (`TZ`, else the system zone) — the rules the agent's process uses.
#[cfg(unix)]
pub fn local_tm(epoch_secs: i64) -> Option<LocalTm> {
    #[allow(
        clippy::useless_conversion,
        reason = "time_t is 32-bit on some targets"
    )]
    let time: nix::libc::time_t = epoch_secs.try_into().ok()?;
    let mut out = std::mem::MaybeUninit::<nix::libc::tm>::zeroed();
    // SAFETY: the reentrant form reads `time` and writes only into `out`;
    // it returns null when the year does not fit.
    if unsafe { nix::libc::localtime_r(&time, out.as_mut_ptr()) }.is_null() {
        return None;
    }
    // SAFETY: zero-initialised (valid for every field) and filled above.
    let tm = unsafe { out.assume_init() };
    let local = LocalTm {
        year: tm.tm_year.checked_add(1900)?,
        month: u8::try_from(tm.tm_mon.checked_add(1)?).ok()?,
        day: u8::try_from(tm.tm_mday).ok()?,
        hour: u8::try_from(tm.tm_hour).ok()?,
        minute: u8::try_from(tm.tm_min).ok()?,
        weekday: u8::try_from(tm.tm_wday).ok()?,
    };
    local.valid().then_some(local)
}

/// The host's local wall clock at `epoch_secs`, by the system time zone's
/// rules for that date (Windows keeps per-year DST rules; `TZ` is a C-runtime
/// convention the agents' runtimes do not follow there).
#[cfg(windows)]
pub fn local_tm(epoch_secs: i64) -> Option<LocalTm> {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{
        FileTimeToSystemTime, GetDynamicTimeZoneInformation, SystemTimeToTzSpecificLocalTimeEx,
        DYNAMIC_TIME_ZONE_INFORMATION,
    };
    /// Seconds from 1601-01-01 (the FILETIME epoch) to 1970-01-01.
    const EPOCH_DELTA_SECS: i64 = 11_644_473_600;
    /// TIME_ZONE_ID_INVALID (timezoneapi.h).
    const TIME_ZONE_ID_INVALID: u32 = u32::MAX;

    let ticks = u64::try_from(epoch_secs.checked_add(EPOCH_DELTA_SECS)?)
        .ok()?
        .checked_mul(10_000_000)?;
    let filetime = FILETIME {
        dwLowDateTime: ticks as u32,
        dwHighDateTime: (ticks >> 32) as u32,
    };
    // SAFETY: plain-data out-parameters, zero-initialised and filled by the
    // calls below; every call's failure is checked before a field is read.
    unsafe {
        let mut utc: SYSTEMTIME = std::mem::zeroed();
        if FileTimeToSystemTime(&filetime, &mut utc) == 0 {
            return None;
        }
        let mut zone: DYNAMIC_TIME_ZONE_INFORMATION = std::mem::zeroed();
        if GetDynamicTimeZoneInformation(&mut zone) == TIME_ZONE_ID_INVALID {
            return None;
        }
        let mut local: SYSTEMTIME = std::mem::zeroed();
        if SystemTimeToTzSpecificLocalTimeEx(&zone, &utc, &mut local) == 0 {
            return None;
        }
        let tm = LocalTm {
            year: i32::from(local.wYear),
            month: u8::try_from(local.wMonth).ok()?,
            day: u8::try_from(local.wDay).ok()?,
            hour: u8::try_from(local.wHour).ok()?,
            minute: u8::try_from(local.wMinute).ok()?,
            weekday: u8::try_from(local.wDayOfWeek).ok()?,
        };
        tm.valid().then_some(tm)
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The inverse of `days_from_civil` (Hinnant's `civil_from_days`).
    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        (yoe + era * 400 + i64::from(month <= 2), month, day)
    }

    /// A UTC wall clock, so no expectation depends on the host's zone.
    fn utc(secs: i64) -> Option<LocalTm> {
        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Some(LocalTm {
            year: year as i32,
            month: month as u8,
            day: day as u8,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            weekday: (days + 4).rem_euclid(7) as u8,
        })
    }

    /// Epoch ms of a UTC wall time.
    fn at(y: i64, mo: i64, d: i64, h: i64, mi: i64) -> u64 {
        ((days_from_civil(y, mo, d) * 1440 + h * 60 + mi) * 60_000) as u64
    }

    fn next(expr: &str, after: u64) -> Option<u64> {
        parse(expr).unwrap().next_after_with(after, utc)
    }

    #[test]
    fn minute_and_hour_steps() {
        let now = at(2026, 10, 15, 17, 0);
        assert_eq!(
            next("* * * * *", now + 59_600),
            Some(at(2026, 10, 15, 17, 1))
        );
        assert_eq!(next("* * * * *", now), Some(now + 60_000), "strictly after");
        assert_eq!(
            next("*/10 * * * *", now + 180_000),
            Some(at(2026, 10, 15, 17, 10))
        );
        assert_eq!(
            next("*/10 * * * *", at(2026, 10, 15, 23, 55)),
            Some(at(2026, 10, 16, 0, 0))
        );
        assert_eq!(next("0 */2 * * *", now), Some(at(2026, 10, 15, 18, 0)));
        assert_eq!(
            next("0 */2 * * *", at(2026, 10, 15, 18, 0)),
            Some(at(2026, 10, 15, 20, 0))
        );
        assert_eq!(
            next("0 */2 * * *", at(2026, 10, 15, 23, 30)),
            Some(at(2026, 10, 16, 0, 0))
        );
    }

    #[test]
    fn daily_crosses_midnight_month_and_year() {
        let daily = "17 3 * * *";
        assert_eq!(
            next(daily, at(2026, 10, 15, 17, 0)),
            Some(at(2026, 10, 16, 3, 17))
        );
        assert_eq!(
            next(daily, at(2026, 10, 31, 4, 0)),
            Some(at(2026, 11, 1, 3, 17))
        );
        assert_eq!(
            next(daily, at(2026, 12, 31, 5, 0)),
            Some(at(2027, 1, 1, 3, 17))
        );
        assert_eq!(
            next("0 0 1 * *", at(2026, 1, 31, 12, 0)),
            Some(at(2026, 2, 1, 0, 0))
        );
        assert_eq!(
            next("0 0 31 * *", at(2026, 4, 1, 0, 0)),
            Some(at(2026, 5, 31, 0, 0))
        );
        assert_eq!(
            next("0 0 1 jan *", at(2026, 2, 1, 0, 0)),
            Some(at(2027, 1, 1, 0, 0))
        );
    }

    #[test]
    fn weekday_names_and_sunday_as_seven() {
        // 2026-10-16 is a Friday.
        let weekdays = "30 9 * * MON-FRI";
        assert_eq!(
            next(weekdays, at(2026, 10, 16, 10, 0)),
            Some(at(2026, 10, 19, 9, 30))
        );
        assert_eq!(parse("0 12 * * sun"), parse("0 12 * * 7"));
        assert_eq!(parse("0 12 * * Sun"), parse("0 12 * * 0"));
        let weekend = "0 0 * * 5-7";
        assert_eq!(
            next(weekend, at(2026, 10, 15, 1, 0)),
            Some(at(2026, 10, 16, 0, 0))
        );
        assert_eq!(
            next(weekend, at(2026, 10, 18, 1, 0)),
            Some(at(2026, 10, 23, 0, 0))
        );
    }

    #[test]
    fn restricted_dom_and_dow_match_either() {
        // The 13th or any Friday; 2026-12-13 is a Sunday.
        let either = "0 0 13 * FRI";
        assert_eq!(
            next(either, at(2026, 10, 15, 0, 0)),
            Some(at(2026, 10, 16, 0, 0))
        );
        assert_eq!(
            next(either, at(2026, 12, 11, 0, 0)),
            Some(at(2026, 12, 13, 0, 0))
        );
        assert_eq!(
            next("0 0 13 * *", at(2026, 12, 11, 0, 0)),
            Some(at(2026, 12, 13, 0, 0))
        );
        assert_eq!(
            next("0 0 * * FRI", at(2026, 12, 11, 0, 0)),
            Some(at(2026, 12, 18, 0, 0))
        );
        // A starred step still counts as `*`, so both must match:
        // 2026-12-21 is the first Monday on day 1, 11, 21 or 31.
        assert_eq!(
            next("0 0 */10 * MON", at(2026, 10, 15, 0, 0)),
            Some(at(2026, 12, 21, 0, 0))
        );
    }

    #[test]
    fn leap_day_and_impossible_dates() {
        assert_eq!(
            next("0 12 * * *", at(2028, 2, 28, 13, 0)),
            Some(at(2028, 2, 29, 12, 0))
        );
        assert_eq!(
            next("0 12 29 2 *", at(2026, 3, 1, 0, 0)),
            Some(at(2028, 2, 29, 12, 0))
        );
        assert_eq!(next("0 0 30 2 *", at(2026, 3, 1, 0, 0)), None);
        assert_eq!(next("0 0 31 4,6,9,11 *", at(2026, 3, 1, 0, 0)), None);
    }

    #[test]
    fn spring_forward_neither_skips_nor_invents_minutes() {
        // UTC+0 → UTC+1 at 01:00 UTC on Sunday 2026-03-29: that day is 23
        // hours long and its 01:00-01:59 never happens.
        let switch = days_from_civil(2026, 3, 29) * 86_400 + 3_600;
        let zone = |secs: i64| utc(if secs >= switch { secs + 3_600 } else { secs });
        let sunday = at(2026, 3, 29, 0, 0);
        let cron = |expr| parse(expr).unwrap().next_after_with(sunday, zone);
        // Monday 00:30 local is Sunday 23:30 UTC.
        assert_eq!(cron("30 0 * * MON"), Some(at(2026, 3, 29, 23, 30)));
        assert_eq!(cron("30 1 * * *"), Some(at(2026, 3, 30, 0, 30)));
    }

    #[test]
    fn parse_accepts_the_standard_forms() {
        let p = |e| parse(e).unwrap();
        assert_eq!(p("0-59/15 * * * *"), p("0,15,30,45 * * * *"));
        assert_eq!(p("5/20 * * * *"), p("5,25,45 * * * *"));
        assert_eq!(p("* * * jan-mar,DEC *"), p("* * * 1-3,12 *"));
        assert_eq!(p("  */5\t*  * * * "), p("*/5 * * * *"));
        assert_eq!(p("* * * * *").weekdays, 0x7f);
    }

    #[test]
    fn parse_rejects_malformed_expressions() {
        let too_long = format!("{}0 * * * *", "0,".repeat(130));
        for bad in [
            "",
            "* * * *",
            "* * * * * *",
            "*/0 * * * *",
            "60 * * * *",
            "* 24 * * *",
            "* * 0 * *",
            "* * * 13 *",
            "* * * * 8",
            "5-1 * * * *",
            "1,,2 * * * *",
            "+5 * * * *",
            "-1 * * * *",
            "1-2-3 * * * *",
            "*/ * * * *",
            "* * * * MONDAY",
            "* * * JAN-MON *",
            "@hourly",
            "a b c d e",
            &too_long,
        ] {
            assert_eq!(parse(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn jitter_reproduces_the_live_offsets() {
        // Every minute: created 17:00:59.600, nextFireAt 17:01:28.370.
        let every = parse("* * * * *").unwrap();
        let created = at(2026, 10, 15, 17, 0) + 59_600;
        let first = at(2026, 10, 15, 17, 1) + 28_370;
        let offset = every.jitter_offset_with(created, first, utc);
        assert_eq!(offset, 28_370);
        let second = every.next_run_with(first + 400, offset, utc);
        assert_eq!(second, Some(at(2026, 10, 15, 17, 2) + 28_370));

        // Daily 03:17 reported as 03:47.
        let daily = parse("17 3 * * *").unwrap();
        let first = at(2026, 10, 16, 3, 47);
        let offset = daily.jitter_offset_with(created, first, utc);
        assert_eq!(offset, 30 * 60_000);
        let second = daily.next_run_with(first + 1_000, offset, utc);
        assert_eq!(second, Some(at(2026, 10, 17, 3, 47)));

        // A fire before the first slot carries no offset.
        assert_eq!(daily.jitter_offset_with(created, created + 1, utc), 0);
    }

    #[test]
    fn local_clock_reads_now() {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap();
        assert!(local_tm(now.as_secs() as i64).is_some());
        let ms = now.as_millis() as u64;
        let next = parse("* * * * *").unwrap().next_after(ms).unwrap();
        assert!(next > ms && next - ms <= 60_000);
    }
}
