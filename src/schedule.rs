//! Allowed windows and daily limits, and evaluating them at a point in time.

use anyhow::{Context, Result, bail};
use chrono::{Datelike, Duration, NaiveDateTime, Timelike};

use crate::state::DayUsage;
use crate::{Cutoff, Status};

/// `[start, end)` in minutes since midnight. `end` may be 1440 (24:00).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, Default)]
pub struct DayRules {
    /// Sorted, non-overlapping. Empty means the day isn't allowed.
    pub windows: Vec<Window>,
    pub limit_secs: u64,
}

impl DayRules {
    pub fn new(mut windows: Vec<Window>, limit_secs: u64) -> Result<Self> {
        windows.sort_by_key(|w| w.start);
        for pair in windows.windows(2) {
            if pair[1].start < pair[0].end {
                bail!(
                    "windows {} and {} overlap",
                    fmt_window(pair[0]),
                    fmt_window(pair[1])
                );
            }
        }
        Ok(DayRules {
            windows,
            limit_secs,
        })
    }
}

/// Monday-first, like chrono's `num_days_from_monday`.
#[derive(Debug, Clone, Default)]
pub struct Schedule {
    pub days: [DayRules; 7],
}

/// "15:30-20:00" -> Window { 930, 1200 }
pub fn parse_window(s: &str) -> Result<Window> {
    let (a, b) = s
        .split_once('-')
        .with_context(|| format!("window {s:?} is not HH:MM-HH:MM"))?;
    let start = parse_hhmm(a.trim()).with_context(|| format!("window {s:?}"))?;
    let end = parse_hhmm(b.trim()).with_context(|| format!("window {s:?}"))?;
    if end <= start {
        bail!("window {s:?} ends before it starts (split windows that cross midnight)");
    }
    Ok(Window { start, end })
}

pub fn parse_hhmm(s: &str) -> Result<u32> {
    let (h, m) = s
        .split_once(':')
        .with_context(|| format!("{s:?} is not HH:MM"))?;
    let h: u32 = h.parse().with_context(|| format!("bad hour in {s:?}"))?;
    let m: u32 = m.parse().with_context(|| format!("bad minute in {s:?}"))?;
    if m >= 60 || h > 24 || (h == 24 && m != 0) {
        bail!("{s:?} is not a valid time");
    }
    Ok(h * 60 + m)
}

pub fn fmt_hhmm(min: u32) -> String {
    format!("{:02}:{:02}", min / 60, min % 60)
}

fn fmt_window(w: Window) -> String {
    format!("{}-{}", fmt_hhmm(w.start), fmt_hhmm(w.end))
}

impl Schedule {
    fn day(&self, dt: NaiveDateTime) -> &DayRules {
        &self.days[dt.weekday().num_days_from_monday() as usize]
    }

    /// Today's windows, with an `extend` override folded in: the extension
    /// runs from the first window's start (or midnight) to `until`, merged
    /// with the scheduled windows.
    fn windows_today(day: &DayRules, extend_until: Option<u32>) -> Vec<Window> {
        let mut windows = day.windows.clone();
        if let Some(until) = extend_until {
            let start = day.windows.first().map_or(0, |w| w.start).min(until);
            windows.push(Window { start, end: until });
            windows.sort_by_key(|w| w.start);
            let mut merged: Vec<Window> = Vec::new();
            for w in windows {
                match merged.last_mut() {
                    Some(last) if w.start <= last.end => last.end = last.end.max(w.end),
                    _ => merged.push(w),
                }
            }
            windows = merged;
        }
        windows
    }

    /// Where we are relative to the schedule at `now`, given today's usage
    /// and any admin overrides.
    pub fn evaluate(&self, user: &str, now: NaiveDateTime, usage: &DayUsage) -> Status {
        let day = self.day(now);
        let used_secs = usage.used_secs;
        let limit_secs = (day.limit_secs as i64 + usage.bonus_secs).max(0) as u64;
        let now_secs = now.time().num_seconds_from_midnight();
        let unlimited = usage.unlimited && !usage.ended;
        let windows = Self::windows_today(day, usage.extend_until);
        let window = windows
            .iter()
            .find(|w| w.start * 60 <= now_secs && now_secs < w.end * 60);
        let remaining_secs = if usage.ended {
            0
        } else if unlimited {
            u64::from(24 * 3600 - now_secs)
        } else {
            match window {
                Some(w) => {
                    let window_left = u64::from(w.end * 60 - now_secs);
                    limit_secs.saturating_sub(used_secs).min(window_left)
                }
                None => 0,
            }
        };
        let cutoff = if remaining_secs > 0 {
            None
        } else if usage.ended || window.is_some() {
            Some(Cutoff::Limit)
        } else {
            Some(Cutoff::Window)
        };
        Status {
            user: user.to_string(),
            allowed_now: remaining_secs > 0,
            remaining_secs,
            used_secs,
            limit_secs,
            bonus_secs: usage.bonus_secs,
            window_end: if unlimited {
                None
            } else {
                window.map(|w| fmt_hhmm(w.end))
            },
            // Later windows today only help if there's time left today.
            next_window: self.next_window(now, usage.ended || used_secs >= limit_secs),
            counting: false,
            unlimited,
            ended: usage.ended,
            extended_until: usage.extend_until.map(fmt_hhmm),
            cutoff,
        }
    }

    /// Start of the next window strictly after `now`, within a week.
    fn next_window(&self, now: NaiveDateTime, skip_today: bool) -> Option<String> {
        let now_min = now.time().num_seconds_from_midnight() / 60;
        let first = if skip_today { 1 } else { 0 };
        for offset in first..=7 {
            let dt = now + Duration::days(offset);
            let day = self.day(dt);
            if day.limit_secs == 0 {
                continue;
            }
            if let Some(w) = day.windows.iter().find(|w| offset > 0 || w.start > now_min) {
                let label = if offset == 0 {
                    "today".to_string()
                } else if offset == 1 {
                    "tomorrow".to_string()
                } else {
                    dt.format("%a").to_string()
                };
                return Some(format!("{label} {}", fmt_hhmm(w.start)));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(y, mo, d)
            .unwrap()
            .and_hms_opt(h, mi, 0)
            .unwrap()
    }

    fn u(used_secs: u64, bonus_secs: i64) -> DayUsage {
        DayUsage {
            used_secs,
            bonus_secs,
            ..DayUsage::new(NaiveDate::from_ymd_opt(2026, 10, 26).unwrap())
        }
    }

    // 2026-10-26 is a Monday.
    fn sched() -> Schedule {
        let mut s = Schedule::default();
        s.days[0] = DayRules::new(vec![parse_window("15:00-20:00").unwrap()], 90 * 60).unwrap();
        s.days[5] = DayRules::new(
            vec![
                parse_window("09:00-12:00").unwrap(),
                parse_window("14:00-21:00").unwrap(),
            ],
            180 * 60,
        )
        .unwrap();
        s
    }

    #[test]
    fn parse_windows() {
        assert_eq!(
            parse_window("15:30-24:00").unwrap(),
            Window {
                start: 930,
                end: 1440
            }
        );
        assert!(parse_window("20:00-15:00").is_err());
        assert!(parse_window("25:00-26:00").is_err());
        assert!(parse_window("9-10").is_err());
    }

    #[test]
    fn inside_window_limited_by_daily_limit() {
        let st = sched().evaluate("b", at(2026, 10, 26, 16, 0), &u(30 * 60, 0));
        assert!(st.allowed_now);
        assert_eq!(st.remaining_secs, 60 * 60);
        assert_eq!(st.window_end.as_deref(), Some("20:00"));
    }

    #[test]
    fn inside_window_limited_by_window_end() {
        let st = sched().evaluate("b", at(2026, 10, 26, 19, 30), &u(0, 0));
        assert_eq!(st.remaining_secs, 30 * 60);
    }

    #[test]
    fn outside_window() {
        let st = sched().evaluate("b", at(2026, 10, 26, 10, 0), &u(0, 0));
        assert!(!st.allowed_now);
        assert_eq!(st.remaining_secs, 0);
        assert_eq!(st.cutoff, Some(Cutoff::Window));
        assert_eq!(st.next_window.as_deref(), Some("today 15:00"));
    }

    #[test]
    fn exhausted_and_bonus() {
        let s = sched();
        let st = s.evaluate("b", at(2026, 10, 26, 17, 0), &u(90 * 60, 0));
        assert!(!st.allowed_now);
        assert_eq!(st.cutoff, Some(Cutoff::Limit));
        let st = s.evaluate("b", at(2026, 10, 26, 17, 0), &u(90 * 60, 15 * 60));
        assert_eq!(st.remaining_secs, 15 * 60);
        assert_eq!(st.cutoff, None);
        assert_eq!(st.limit_secs, 105 * 60);
        // Negative bonus can't push the limit below zero.
        let st = s.evaluate("b", at(2026, 10, 26, 17, 0), &u(0, -1_000_000));
        assert_eq!(st.limit_secs, 0);
    }

    #[test]
    fn window_end_wins_over_used_up_limit() {
        // Window over at 20:00 and the minutes used up too: the window counts.
        let st = sched().evaluate("b", at(2026, 10, 26, 20, 0), &u(90 * 60, 0));
        assert_eq!(st.cutoff, Some(Cutoff::Window));
    }

    #[test]
    fn next_window_skips_disallowed_days() {
        // Monday 21:00 -> next allowed is Saturday 09:00.
        let st = sched().evaluate("b", at(2026, 10, 26, 21, 0), &u(0, 0));
        assert_eq!(st.next_window.as_deref(), Some("Sat 09:00"));
        // Saturday between windows.
        let st = sched().evaluate("b", at(2026, 10, 31, 12, 30), &u(0, 0));
        assert_eq!(st.next_window.as_deref(), Some("today 14:00"));
        // ...but not once today's limit is used up.
        let st = sched().evaluate("b", at(2026, 10, 31, 12, 30), &u(180 * 60, 0));
        assert_eq!(st.next_window.as_deref(), Some("Mon 15:00"));
    }

    #[test]
    fn unlimited_ignores_windows_and_limit() {
        let usage = DayUsage {
            unlimited: true,
            ..u(10 * 3600, 0)
        };
        let st = sched().evaluate("b", at(2026, 10, 26, 22, 0), &usage);
        assert!(st.allowed_now && st.unlimited);
        assert_eq!(st.remaining_secs, 2 * 3600); // until midnight
    }

    #[test]
    fn ended_beats_everything() {
        let usage = DayUsage {
            unlimited: true,
            ended: true,
            ..u(0, 3600)
        };
        let st = sched().evaluate("b", at(2026, 10, 26, 16, 0), &usage);
        assert!(!st.allowed_now && !st.unlimited);
        assert_eq!(st.cutoff, Some(Cutoff::Limit));
        // Today is over, so the next window is another day.
        assert_eq!(st.next_window.as_deref(), Some("Sat 09:00"));
    }

    #[test]
    fn extend_after_window_and_through_gap() {
        // Monday 15-20; extended to 21:30, it's 20:30 with 30 min used.
        let usage = DayUsage {
            extend_until: Some(21 * 60 + 30),
            ..u(30 * 60, 0)
        };
        let st = sched().evaluate("b", at(2026, 10, 26, 20, 30), &usage);
        assert!(st.allowed_now);
        assert_eq!(st.remaining_secs, 60 * 60);
        assert_eq!(st.window_end.as_deref(), Some("21:30"));
        assert_eq!(st.extended_until.as_deref(), Some("21:30"));
        // Still bounded by the daily limit.
        let usage = DayUsage {
            extend_until: Some(21 * 60 + 30),
            ..u(90 * 60, 0)
        };
        assert!(
            !sched()
                .evaluate("b", at(2026, 10, 26, 20, 30), &usage)
                .allowed_now
        );
        // Saturday 9-12 + 14-21: extending to 13:00 fills the gap until 13:00.
        let usage = DayUsage {
            extend_until: Some(13 * 60),
            ..u(0, 0)
        };
        let st = sched().evaluate("b", at(2026, 10, 31, 12, 30), &usage);
        assert_eq!(st.remaining_secs, 30 * 60);
        assert!(
            !sched()
                .evaluate("b", at(2026, 10, 31, 13, 30), &usage)
                .allowed_now
        );
    }
}
