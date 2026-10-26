//! Per-user usage for the current day, persisted in /var/lib/screentime.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

/// State file path. systemd's StateDirectory= sets $STATE_DIRECTORY.
pub fn path() -> PathBuf {
    let dir = std::env::var_os("SCREENTIME_STATE_DIR")
        .or_else(|| std::env::var_os("STATE_DIRECTORY"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/var/lib/screentime"));
    dir.join("state.json")
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
    /// `screentime extend`: allowed until this minute of the day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extend_until: Option<u32>,
}

impl DayUsage {
    pub fn new(date: NaiveDate) -> Self {
        DayUsage {
            date,
            used_secs: 0,
            bonus_secs: 0,
            unlimited: false,
            ended: false,
            extend_until: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
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

    /// Atomic write: temp file + rename.
    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        let mut f =
            std::fs::File::create(&tmp).with_context(|| format!("creating {}", tmp.display()))?;
        f.write_all(serde_json::to_string_pretty(self)?.as_bytes())?;
        f.sync_all()?;
        std::fs::rename(&tmp, path).with_context(|| format!("renaming to {}", path.display()))
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
}
