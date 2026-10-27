//! D-Bus proxies: our daemon, logind, and desktop notifications.

use std::collections::HashMap;

use zbus::zvariant::{OwnedObjectPath, Value};

/// System bus normally; SCREENTIME_BUS=session for testing without root.
pub async fn daemon_bus() -> zbus::Result<zbus::Connection> {
    if std::env::var("SCREENTIME_BUS").as_deref() == Ok("session") {
        zbus::Connection::session().await
    } else {
        zbus::Connection::system().await
    }
}

#[zbus::proxy(
    interface = "org.screentime.Daemon1",
    default_service = "org.screentime.Daemon1",
    default_path = "/org/screentime/Daemon1"
)]
pub trait Daemon {
    /// Agent -> daemon: still alive, and whether the user is idle.
    fn heartbeat(&self, idle: bool) -> zbus::Result<()>;
    /// JSON-encoded `Status`. Empty user means the caller.
    fn status(&self, user: &str) -> zbus::Result<String>;
    /// Root only. Adds (or with a negative value removes) time for today.
    fn add_bonus(&self, user: &str, secs: i64) -> zbus::Result<String>;
    /// Root only. No limits or windows for the rest of today.
    fn set_unlimited(&self, user: &str, on: bool) -> zbus::Result<String>;
    /// Root only. Allowed until "HH:MM" today, past the scheduled windows.
    fn extend_until(&self, user: &str, until: &str) -> zbus::Result<String>;
    /// Root only. No more time today.
    fn end_today(&self, user: &str) -> zbus::Result<String>;
    /// Root only. Drops today's off/extend/end overrides; bonus stays.
    fn clear_overrides(&self, user: &str) -> zbus::Result<String>;
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
pub trait LoginManager {
    /// (session id, uid, user name, seat, object path)
    #[allow(clippy::type_complexity)]
    fn list_sessions(&self) -> zbus::Result<Vec<(String, u32, String, String, OwnedObjectPath)>>;
    fn terminate_user(&self, uid: u32) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    /// systemd >= 248; see `SKIP_INHIBITORS`.
    fn power_off_with_flags(&self, flags: u64) -> zbus::Result<()>;
}

/// `PowerOffWithFlags` flag (systemd >= 256): ignore block inhibitors, which
/// newer logind otherwise enforces even for root. The managed user may be
/// allowed to take inhibitor locks, so they mustn't be able to stop this.
pub const SKIP_INHIBITORS: u64 = 1 << 4;

#[zbus::proxy(
    interface = "org.freedesktop.login1.Session",
    default_service = "org.freedesktop.login1"
)]
pub trait LoginSession {
    #[zbus(property)]
    fn active(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn locked_hint(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn class(&self) -> zbus::Result<String>;
    fn lock(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.freedesktop.Notifications",
    default_service = "org.freedesktop.Notifications",
    default_path = "/org/freedesktop/Notifications"
)]
pub trait Notifications {
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        app_name: &str,
        replaces_id: u32,
        app_icon: &str,
        summary: &str,
        body: &str,
        actions: &[&str],
        hints: HashMap<&str, Value<'_>>,
        expire_timeout: i32,
    ) -> zbus::Result<u32>;
}
