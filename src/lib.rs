//! Shared pieces of the screentime daemon, agent and CLI.

pub mod config;
pub mod dbus;
pub mod duration;
pub mod icon;
pub mod schedule;
pub mod state;

use serde::{Deserialize, Serialize};

/// Well-known name and object path of the daemon on the system bus.
pub const BUS_NAME: &str = "org.screentime.Daemon1";
pub const OBJECT_PATH: &str = "/org/screentime/Daemon1";

/// What the daemon reports for one user (sent over D-Bus as JSON).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub user: String,
    /// Inside an allowed window with time left.
    pub allowed_now: bool,
    /// Seconds until logout: the smaller of time left today and time left in
    /// the current window.
    pub remaining_secs: u64,
    pub used_secs: u64,
    /// Today's limit including bonus time.
    pub limit_secs: u64,
    pub bonus_secs: i64,
    /// "HH:MM" end of the current window, if inside one.
    pub window_end: Option<String>,
    /// "Sat 09:00" start of the next window, if any in the coming week.
    pub next_window: Option<String>,
    /// Time is being counted right now.
    pub counting: bool,
    /// Admin overrides for today.
    #[serde(default)]
    pub unlimited: bool,
    #[serde(default)]
    pub ended: bool,
    #[serde(default)]
    pub extended_until: Option<String>,
    /// Why `allowed_now` is false, if it is.
    #[serde(default)]
    pub cutoff: Option<Cutoff>,
}

/// What ended a user's time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cutoff {
    /// Today's minutes are used up, or the admin ended the day.
    Limit,
    /// Outside every allowed window.
    Window,
}
