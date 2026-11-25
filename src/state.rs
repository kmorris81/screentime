//! Per-user usage for the current day, persisted in /var/lib/screentime.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::schedule::{Window, fmt_window};

/// State directory. systemd's StateDirectory= sets $STATE_DIRECTORY.
pub fn dir() -> PathBuf {
    std::env::var_os("SCREENTIME_STATE_DIR")
        .or_else(|| std::env::var_os("STATE_DIRECTORY"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/screentime"))
}

/// State file path.
pub fn path() -> PathBuf {
    dir().join("state.json")
}

/// Atomic write: temp file + rename.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f =
        std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))
}

/// An admin change to today's overrides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    /// Add (or with a negative value remove) time.
    Bonus(i64),
    /// No limits or windows for the rest of the day.
    Off,
    /// An extra window for today (merged with the scheduled ones).
    Allow(Window),
    /// No more time today.
    End,
    /// Drop off/allow/end; bonus stays.
    Clear,
}

impl Change {
    /// Rejects a window that ends before now.
    pub fn check(&self, now: chrono::NaiveTime) -> Result<()> {
        let now_min = chrono::Timelike::num_seconds_from_midnight(&now) / 60;
        match *self {
            Change::Allow(w) if w.end <= now_min => {
                anyhow::bail!("{} has already ended", fmt_window(w))
            }
            _ => Ok(()),
        }
    }

    pub fn apply(&self, u: &mut DayUsage) {
        match *self {
            Change::Bonus(secs) => u.bonus_secs += secs,
            Change::Off => {
                u.unlimited = true;
                u.ended = false;
            }
            Change::Allow(w) => {
                u.extra_windows.push(w);
                u.ended = false;
            }
            Change::End => {
                u.ended = true;
                u.unlimited = false;
            }
            Change::Clear => {
                u.unlimited = false;
                u.ended = false;
                u.extra_windows.clear();
            }
        }
    }

    /// For the log: "bonus +30m", "limits off for today", ...
    pub fn describe(&self) -> String {
        match *self {
            Change::Bonus(secs) => {
                let sign = if secs < 0 { "-" } else { "+" };
                format!(
                    "bonus {sign}{}",
                    crate::duration::format(secs.unsigned_abs())
                )
            }
            Change::Off => "limits off for today".into(),
            Change::Allow(w) => format!("allowed {} today", fmt_window(w)),
            Change::End => "time ended for today".into(),
            Change::Clear => "overrides cleared".into(),
        }
    }
}

/// One user's day. Admin overrides live here too, so they reset at midnight.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DayUsage {
    pub date: NaiveDate,
    pub used_secs: u64,
    pub bonus_secs: i64,
    /// `screentime off`: no limits or windows for the rest of the day.
    #[serde(default)]
    pub unlimited: bool,
    /// `screentime end`: no more time today, whatever else is set.
    #[serde(default)]
    pub ended: bool,
    /// `screentime allow`: extra windows for today.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_windows: Vec<Window>,
}

impl DayUsage {
    pub fn new(date: NaiveDate) -> Self {
        DayUsage {
            date,
            used_secs: 0,
            bonus_secs: 0,
            unlimited: false,
            ended: false,
            extra_windows: Vec::new(),
        }
    }

    /// Take `from`'s admin overrides; keep our own usage.
    pub fn copy_overrides(&mut self, from: &DayUsage) {
        self.bonus_secs = from.bonus_secs;
        self.unlimited = from.unlimited;
        self.ended = from.ended;
        self.extra_windows = from.extra_windows.clone();
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct State {
    pub users: BTreeMap<String, DayUsage>,
}

impl State {
    /// A missing file is an empty state; anything else unreadable is an error.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        write_atomic(path, serde_json::to_string_pretty(self)?.as_bytes())
    }

    /// Today's usage for `user`, starting fresh when the date has changed.
    pub fn today(&mut self, user: &str, today: NaiveDate) -> &mut DayUsage {
        let usage = self
            .users
            .entry(user.to_string())
            .or_insert_with(|| DayUsage::new(today));
        if usage.date != today {
            *usage = DayUsage::new(today);
        }
        usage
    }

    /// Read-only view of today's usage (zero if none recorded today).
    pub fn peek(&self, user: &str, today: NaiveDate) -> DayUsage {
        match self.users.get(user) {
            Some(u) if u.date == today => u.clone(),
            _ => DayUsage::new(today),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rolls_over_and_round_trips() {
        let d1 = NaiveDate::from_ymd_opt(2026, 10, 26).unwrap();
        let d2 = d1.succ_opt().unwrap();
        let mut st = State::default();
        st.today("b", d1).used_secs = 100;
        st.today("b", d1).bonus_secs = 60;
        assert_eq!(st.peek("b", d1).used_secs, 100);
        assert_eq!(st.peek("b", d2).used_secs, 0);
        assert_eq!(st.today("b", d2).bonus_secs, 0);

        let dir = std::env::temp_dir().join(format!("screentime-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("state.json");
        st.save(&p).unwrap();
        let back = State::load(&p).unwrap();
        assert_eq!(back.users["b"], st.users["b"]);
        assert!(
            State::load(&dir.join("missing.json"))
                .unwrap()
                .users
                .is_empty()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn noon() -> chrono::NaiveTime {
        chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap()
    }

    #[test]
    fn changes() {
        let mut u = DayUsage::new(NaiveDate::from_ymd_opt(2026, 10, 26).unwrap());
        Change::Bonus(600).apply(&mut u);
        Change::End.apply(&mut u);
        assert!(u.ended);
        Change::Allow(Window {
            start: 600,
            end: 720,
        })
        .apply(&mut u);
        assert!(!u.ended);
        assert_eq!(u.extra_windows.len(), 1);
        Change::Clear.apply(&mut u);
        assert_eq!(u.bonus_secs, 600);
        assert!(u.extra_windows.is_empty());
        let w = |start, end| Change::Allow(Window { start, end });
        assert!(w(600, 720).check(noon()).is_err());
        assert!(w(600, 721).check(noon()).is_ok());
        let json = serde_json::to_string(&Change::Bonus(60)).unwrap();
        assert_eq!(json, r#"{"bonus":60}"#);
        assert_eq!(
            serde_json::from_str::<Change>(r#""off""#).unwrap(),
            Change::Off
        );
    }
}
