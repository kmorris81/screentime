//! screentime-agent: runs in the managed user's desktop session.
//!
//! - Watches for keyboard/mouse inactivity with the Wayland ext-idle-notify
//!   protocol and reports it to screentimed in periodic heartbeats.
//! - Shows the time left as a tray icon (StatusNotifierItem).
//! - Sends desktop notifications as time runs low.
//! - When time is up, logs out or shuts down through Plasma (like the Log
//!   Out / Shut Down buttons), so KWin exits cleanly instead of being killed,
//!   or locks the screen: `limit_action` when the day's minutes run out,
//!   `window_end_action` when the allowed window ends. The daemon only
//!   does it the hard way if that didn't happen.
//!
//! If anything here fails, the daemon simply stops hearing "idle" and keeps
//! counting, so the agent can't be used to dodge the limit.

use std::collections::{BTreeSet, HashMap};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ksni::TrayMethods;
use tokio::sync::watch;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::ext::idle_notify::v1::client::{
    ext_idle_notification_v1::{self, ExtIdleNotificationV1},
    ext_idle_notifier_v1::ExtIdleNotifierV1,
};
use zbus::zvariant::Value;

use screentime::config::{self, Config, TimeUpAction};
use screentime::dbus::{DaemonProxy, NotificationsProxy, daemon_bus};
use screentime::{Cutoff, Status, duration, icon};

const POLL: Duration = Duration::from_secs(10);
const WARN_MINUTES: [u64; 3] = [10, 5, 1];

// ---------------------------------------------------------------- idle ----

struct IdleState {
    tx: watch::Sender<bool>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for IdleState {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotifierV1, ()> for IdleState {
    fn event(
        _: &mut Self,
        _: &ExtIdleNotifierV1,
        _: <ExtIdleNotifierV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtIdleNotificationV1, ()> for IdleState {
    fn event(
        state: &mut Self,
        _: &ExtIdleNotificationV1,
        event: ext_idle_notification_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_idle_notification_v1::Event::Idled => {
                let _ = state.tx.send(true);
            }
            ext_idle_notification_v1::Event::Resumed => {
                let _ = state.tx.send(false);
            }
            _ => {}
        }
    }
}

/// Blocks forever, sending idle/active transitions on `tx`.
fn watch_idle(timeout: Duration, input_only: bool, tx: watch::Sender<bool>) -> Result<()> {
    let conn = Connection::connect_to_env().context("connecting to Wayland")?;
    let (globals, mut queue) = registry_queue_init::<IdleState>(&conn)?;
    let qh = queue.handle();
    let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ())?;
    let notifier: ExtIdleNotifierV1 = globals
        .bind(&qh, 1..=2, ())
        .context("compositor lacks ext_idle_notifier_v1")?;
    let ms = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);
    // v2's input-idle notification ignores idle inhibitors (e.g. a playing
    // video), so only real keyboard/mouse input counts as activity.
    let _notification = if input_only && notifier.version() >= 2 {
        notifier.get_input_idle_notification(ms, &seat, &qh, ())
    } else {
        if input_only {
            eprintln!("compositor only has ext_idle_notifier_v1 v1; idle inhibitors apply");
        }
        notifier.get_idle_notification(ms, &seat, &qh, ())
    };
    let mut state = IdleState { tx };
    loop {
        queue.blocking_dispatch(&mut state)?;
    }
}

// ---------------------------------------------------------------- tray ----

#[derive(Debug, Default)]
struct Tray {
    status: Option<Status>,
}

impl Tray {
    fn summary(&self) -> String {
        match &self.status {
            None => "Screen time: waiting for the service".into(),
            Some(st) if st.unlimited => "No screen-time limit today".into(),
            Some(st) if st.allowed_now => {
                format!("{} left today", duration::format(st.remaining_secs))
            }
            Some(_) => "No screen time left right now".into(),
        }
    }

    fn details(&self) -> Vec<String> {
        let Some(st) = &self.status else {
            return vec![];
        };
        let mut lines = vec![format!(
            "Used {} of {}",
            duration::format(st.used_secs),
            duration::format(st.limit_secs)
        )];
        if let Some(end) = &st.window_end {
            lines.push(format!("Allowed until {end}"));
        }
        if !st.allowed_now
            && let Some(next) = &st.next_window
        {
            lines.push(format!("Next: {next}"));
        }
        if st.allowed_now && !st.counting {
            lines.push("Paused (idle)".into());
        }
        lines
    }
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "screentime".into()
    }
    fn title(&self) -> String {
        "Screen time".into()
    }
    fn category(&self) -> ksni::Category {
        ksni::Category::SystemServices
    }
    fn icon_name(&self) -> String {
        // Only used if the host can't show pixmaps.
        "chronometer".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        let (remaining, unlimited) = self
            .status
            .as_ref()
            .map_or((0, false), |s| (s.remaining_secs, s.unlimited));
        let p = icon::render(remaining, unlimited);
        vec![ksni::Icon {
            width: p.size,
            height: p.size,
            data: p.data,
        }]
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.summary(),
            description: self.details().join("\n"),
            ..Default::default()
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        std::iter::once(self.summary())
            .chain(self.details())
            .map(|label| {
                StandardItem {
                    label,
                    enabled: false,
                    ..Default::default()
                }
                .into()
            })
            .collect()
    }
}

// ------------------------------------------------------- notifications ----

struct Warnings {
    limit_action: TimeUpAction,
    window_end_action: TimeUpAction,
    /// Seconds between running out and the action, for the message.
    grace_secs: u64,
    sent: BTreeSet<u64>,
    /// The time-up action we've already announced.
    out_sent: Option<TimeUpAction>,
}

impl Warnings {
    fn new(cfg: &Config) -> Self {
        Warnings {
            limit_action: cfg.limit_action,
            window_end_action: cfg.window_end_action,
            grace_secs: cfg.logout_grace_secs,
            sent: BTreeSet::new(),
            out_sent: None,
        }
    }

    fn action(&self, cutoff: Cutoff) -> TimeUpAction {
        match cutoff {
            Cutoff::Limit => self.limit_action,
            Cutoff::Window => self.window_end_action,
        }
    }

    /// The notification due for this status, if any: (summary, body, critical).
    fn due(&mut self, st: &Status) -> Option<(String, String, bool)> {
        if !st.allowed_now {
            let action = self.action(st.cutoff.unwrap_or(Cutoff::Limit));
            if self.out_sent == Some(action) {
                return None;
            }
            self.out_sent = Some(action);
            let when = match self.grace_secs {
                0 => "now".to_string(),
                s => format!("in {s} seconds"),
            };
            return Some((
                "Screen time is up".into(),
                match action {
                    TimeUpAction::Shutdown => {
                        format!("The computer will shut down {when}. Save your work!")
                    }
                    TimeUpAction::Logout => format!("You'll be logged out {when}. Save your work!"),
                    TimeUpAction::Lock => format!("The screen will lock {when}."),
                },
                true,
            ));
        }
        self.out_sent = None;
        // Forget warnings that no longer apply (e.g. after bonus time).
        self.sent.retain(|m| st.remaining_secs <= m * 60);
        let minute = WARN_MINUTES
            .iter()
            .copied()
            .filter(|m| st.remaining_secs <= m * 60 && !self.sent.contains(m))
            .min()?;
        // Mark this and any larger threshold so we don't fire them all at once.
        self.sent
            .extend(WARN_MINUTES.iter().filter(|m| **m >= minute));
        let left = st.remaining_secs.div_ceil(60);
        let plural = if left == 1 { "" } else { "s" };
        let (reason, cutoff) = match &st.window_end {
            Some(end) if st.remaining_secs < st.limit_secs.saturating_sub(st.used_secs) => {
                (format!("Allowed time ends at {end}."), Cutoff::Window)
            }
            _ => ("Time to start wrapping up.".to_string(), Cutoff::Limit),
        };
        let then = match self.action(cutoff) {
            TimeUpAction::Shutdown => "Then the computer shuts down.",
            TimeUpAction::Logout => "Then you'll be logged out.",
            TimeUpAction::Lock => "Then the screen locks.",
        };
        Some((
            format!("{left} minute{plural} of screen time left"),
            format!("{reason} {then}"),
            left <= 1,
        ))
    }
}

async fn notify(conn: &zbus::Connection, summary: &str, body: &str, critical: bool) -> Result<()> {
    let proxy = NotificationsProxy::new(conn).await?;
    let mut hints = HashMap::new();
    hints.insert("urgency", Value::U8(if critical { 2 } else { 1 }));
    proxy
        .notify(
            "Screen time",
            0,
            "chronometer",
            summary,
            body,
            &[],
            hints,
            -1,
        )
        .await?;
    Ok(())
}

/// Ask Plasma to end the session, the same as Log Out (or Shut Down) in the
/// menu. Falls back to the older ksmserver interface.
async fn request_logout(conn: &zbus::Connection, shutdown: bool) -> Result<()> {
    let method = if shutdown {
        "logoutAndShutdown"
    } else {
        "logout"
    };
    let plasma = conn
        .call_method(
            Some("org.kde.Shutdown"),
            "/Shutdown",
            Some("org.kde.Shutdown"),
            method,
            &(),
        )
        .await;
    if let Err(e) = plasma {
        // logout(confirm = no, type = halt or none, mode = default)
        let sdtype = if shutdown { 2i32 } else { 0i32 };
        conn.call_method(
            Some("org.kde.ksmserver"),
            "/KSMServer",
            Some("org.kde.KSMServerInterface"),
            "logout",
            &(0i32, sdtype, 0i32),
        )
        .await
        .with_context(|| format!("org.kde.Shutdown.{method} failed ({e}); ksmserver too"))?;
    }
    Ok(())
}

/// Lock the screen through the desktop's screensaver interface.
async fn request_lock(conn: &zbus::Connection) -> Result<()> {
    conn.call_method(
        Some("org.freedesktop.ScreenSaver"),
        "/ScreenSaver",
        Some("org.freedesktop.ScreenSaver"),
        "Lock",
        &(),
    )
    .await
    .context("org.freedesktop.ScreenSaver.Lock")?;
    Ok(())
}

async fn time_up(conn: &zbus::Connection, action: TimeUpAction) -> Result<()> {
    match action {
        TimeUpAction::Shutdown => request_logout(conn, true).await,
        TimeUpAction::Logout => request_logout(conn, false).await,
        TimeUpAction::Lock => request_lock(conn).await,
    }
}

// ---------------------------------------------------------------- main ----

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cfg = Config::load(&config::path())?;

    let (idle_tx, mut idle_rx) = watch::channel(false);
    let timeout = Duration::from_secs(cfg.idle_timeout_secs);
    let input_only = cfg.input_idle_only;
    std::thread::spawn(move || {
        if let Err(e) = watch_idle(timeout, input_only, idle_tx) {
            // Without idle detection the user is simply always "active".
            eprintln!("idle detection unavailable: {e:#}");
        }
    });

    let system = daemon_bus().await.context("connecting to the system bus")?;
    let daemon = DaemonProxy::new(&system).await?;
    let session = zbus::Connection::session()
        .await
        .context("connecting to the session bus")?;
    // Don't fail if the tray host isn't up yet (plasmashell starting or
    // restarting); ksni registers whenever it appears. Heartbeats matter more.
    let tray = match Tray::default().assume_sni_available(true).spawn().await {
        Ok(handle) => Some(handle),
        Err(e) => {
            eprintln!("tray icon unavailable: {e}");
            None
        }
    };
    let grace = Duration::from_secs(cfg.logout_grace_secs);
    let mut warnings = Warnings::new(&cfg);
    // When we first saw "no time left" and the action that calls for, and
    // whether we've taken it.
    let mut out: Option<(Instant, TimeUpAction)> = None;
    let mut acted = false;

    loop {
        let idle = *idle_rx.borrow_and_update();
        if let Err(e) = daemon.heartbeat(idle).await {
            eprintln!("heartbeat: {e}");
        }
        let status = match daemon.status("").await {
            Ok(json) => serde_json::from_str::<Status>(&json).ok(),
            Err(e) => {
                eprintln!("status: {e}");
                None
            }
        };
        if let Some(st) = &status
            && let Some((summary, body, critical)) = warnings.due(st)
            && let Err(e) = notify(&session, &summary, &body, critical).await
        {
            eprintln!("notify: {e:#}");
        }
        match &status {
            Some(st) if !st.allowed_now => {
                let cutoff = st.cutoff.unwrap_or(Cutoff::Limit);
                let action = warnings.action(cutoff);
                // A different action (the window ended while locked for the
                // limit, say) gets its own grace period, like the daemon.
                let since = match out {
                    Some((since, pending)) if pending == action => since,
                    _ => {
                        acted = false;
                        out.insert((Instant::now(), action)).0
                    }
                };
                if !acted && since.elapsed() >= grace {
                    acted = true;
                    eprintln!("time is up ({cutoff:?}); {action:?}");
                    if let Err(e) = time_up(&session, action).await {
                        eprintln!("time up: {e:#}");
                    }
                }
            }
            Some(_) => {
                out = None;
                acted = false;
            }
            // Daemon unreachable: don't act on stale information.
            None => {}
        }
        if let Some(tray) = &tray {
            tray.update(|t| t.status = status).await;
        }

        // Next poll, or sooner when the idle state flips or the action is due.
        let wait = match out {
            Some((since, _)) if !acted => grace.saturating_sub(since.elapsed()).min(POLL),
            _ => POLL,
        };
        let _ = tokio::time::timeout(wait, idle_rx.changed()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(remaining_min: u64, allowed: bool) -> Status {
        Status {
            allowed_now: allowed,
            remaining_secs: remaining_min * 60,
            used_secs: 0,
            limit_secs: 120 * 60,
            ..Default::default()
        }
    }

    fn warnings() -> Warnings {
        Warnings::new(&Config::parse("").unwrap())
    }

    fn out(cutoff: Cutoff) -> Status {
        Status {
            cutoff: Some(cutoff),
            ..st(0, false)
        }
    }

    #[test]
    fn warns_once_per_threshold() {
        let mut w = warnings();
        assert!(w.due(&st(30, true)).is_none());
        assert!(w.due(&st(10, true)).unwrap().0.starts_with("10 minutes"));
        assert!(w.due(&st(9, true)).is_none());
        assert!(w.due(&st(5, true)).unwrap().0.starts_with("5 minutes"));
        let one = w.due(&st(1, true)).unwrap();
        assert!(one.0.starts_with("1 minute ") && one.2);
        let up = w.due(&out(Cutoff::Limit)).unwrap();
        assert!(up.0.contains("up") && up.1.contains("logged out"));
        assert!(w.due(&out(Cutoff::Limit)).is_none());
    }

    #[test]
    fn says_what_happens_for_each_cutoff() {
        let mut w = warnings();
        // 10 minutes to the window end, more than that left of the limit.
        let window_soon = Status {
            window_end: Some("20:00".into()),
            ..st(10, true)
        };
        let body = w.due(&window_soon).unwrap().1;
        assert!(
            body.contains("20:00") && body.contains("shuts down"),
            "{body}"
        );
        assert!(w.due(&st(5, true)).unwrap().1.contains("logged out"));
    }

    #[test]
    fn window_end_after_limit_announces_again() {
        // E.g. locked for the limit, then the window ends: a new action.
        let mut w = Warnings {
            limit_action: TimeUpAction::Lock,
            ..warnings()
        };
        assert!(w.due(&out(Cutoff::Limit)).unwrap().1.contains("lock"));
        assert!(w.due(&out(Cutoff::Limit)).is_none());
        assert!(w.due(&out(Cutoff::Window)).unwrap().1.contains("shut down"));
    }

    #[test]
    fn jumping_straight_to_low_time_sends_one_warning() {
        let mut w = warnings();
        assert!(w.due(&st(3, true)).unwrap().0.starts_with("3 minutes"));
        assert!(w.due(&st(3, true)).is_none());
        assert!(w.due(&st(1, true)).unwrap().0.starts_with("1 minute"));
    }

    #[test]
    fn bonus_rearms_warnings() {
        let mut w = warnings();
        w.due(&st(10, true));
        w.due(&st(30, true)); // bonus added
        assert!(w.due(&st(10, true)).is_some());
    }
}
