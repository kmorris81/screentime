//! screentimed: system daemon that counts and enforces screen time.
//!
//! Every tick it asks logind which managed users have a foreground, unlocked
//! session. Time is counted unless that user's agent reports them idle. With
//! no (or a stale) heartbeat the user counts as active, so killing the agent
//! never buys extra time. Out of time or outside a window, the agent logs the
//! user out cleanly after a grace period; if sessions are still open a bit
//! later, the daemon terminates them through logind.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use tokio::sync::Mutex;
use zbus::message::Header;
use zbus::proxy::CacheProperties;
use zbus::{Connection, fdo};

use screentime::config::{self, Config};
use screentime::dbus::{LoginManagerProxy, LoginSessionProxy};
use screentime::schedule::{fmt_hhmm, parse_hhmm};
use screentime::state::{self, DayUsage, State};
use screentime::{BUS_NAME, OBJECT_PATH, Status, duration};

const TICK: Duration = Duration::from_secs(5);
const SAVE_EVERY: Duration = Duration::from_secs(30);

struct Heartbeat {
    at: Instant,
    idle: bool,
}

#[derive(Default)]
struct Runtime {
    counting: bool,
    /// When the user ran out of time (or left the window) with sessions open.
    out_since: Option<Instant>,
}

struct Shared {
    cfg: Config,
    /// Managed user name -> uid.
    uids: HashMap<String, u32>,
    state: State,
    state_path: PathBuf,
    dirty: bool,
    heartbeats: HashMap<u32, Heartbeat>,
    runtime: HashMap<String, Runtime>,
}

impl Shared {
    fn status(&self, user: &str) -> Option<Status> {
        let schedule = self.cfg.users.get(user)?;
        let now = Local::now().naive_local();
        let usage = self.state.peek(user, now.date());
        let mut st = schedule.evaluate(user, now, &usage);
        st.counting = self.runtime.get(user).is_some_and(|r| r.counting);
        Some(st)
    }

    fn save(&mut self) {
        match self.state.save(&self.state_path) {
            Ok(()) => self.dirty = false,
            Err(e) => eprintln!("saving state: {e:#}"),
        }
    }
}

/// uid and pid of the process that sent `hdr`, as vouched for by the bus.
async fn caller(conn: &Connection, hdr: &Header<'_>) -> fdo::Result<(u32, u32)> {
    let sender = hdr
        .sender()
        .ok_or_else(|| fdo::Error::AccessDenied("no sender".into()))?;
    let dbus = fdo::DBusProxy::new(conn).await?;
    let uid = dbus.get_connection_unix_user(sender.clone().into()).await?;
    let pid = dbus
        .get_connection_unix_process_id(sender.clone().into())
        .await?;
    Ok((uid, pid))
}

struct Iface {
    shared: Arc<Mutex<Shared>>,
}

impl Iface {
    /// Root-only change to `user`'s today; returns the new status as JSON.
    async fn admin(
        &self,
        conn: &Connection,
        hdr: &Header<'_>,
        user: &str,
        what: &str,
        change: impl FnOnce(&mut DayUsage),
    ) -> fdo::Result<String> {
        let (uid, _) = caller(conn, hdr).await?;
        if uid != 0 {
            return Err(fdo::Error::AccessDenied(
                "this needs root (use sudo)".into(),
            ));
        }
        let mut sh = self.shared.lock().await;
        if !sh.cfg.users.contains_key(user) {
            return Err(fdo::Error::Failed(format!("{user} is not a managed user")));
        }
        change(sh.state.today(user, Local::now().date_naive()));
        sh.save();
        eprintln!("{user}: {what}");
        let st = sh.status(user).expect("checked above");
        serde_json::to_string(&st).map_err(|e| fdo::Error::Failed(e.to_string()))
    }
}

#[zbus::interface(name = "org.screentime.Daemon1")]
impl Iface {
    async fn heartbeat(
        &self,
        idle: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<()> {
        let (uid, pid) = caller(conn, &hdr).await?;
        let mut sh = self.shared.lock().await;
        if let Some(agent) = &sh.cfg.agent_path {
            let exe = std::fs::read_link(format!("/proc/{pid}/exe")).ok();
            if exe.as_ref() != Some(agent) {
                eprintln!("rejected heartbeat from uid {uid} pid {pid} ({exe:?})");
                return Err(fdo::Error::AccessDenied("not the screentime agent".into()));
            }
        }
        sh.heartbeats.insert(
            uid,
            Heartbeat {
                at: Instant::now(),
                idle,
            },
        );
        Ok(())
    }

    async fn status(
        &self,
        user: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let sh = self.shared.lock().await;
        let user = if user.is_empty() {
            let (uid, _) = caller(conn, &hdr).await?;
            sh.uids
                .iter()
                .find(|(_, u)| **u == uid)
                .map(|(name, _)| name.clone())
                .ok_or_else(|| fdo::Error::Failed("caller is not a managed user".into()))?
        } else {
            user.to_string()
        };
        let st = sh
            .status(&user)
            .ok_or_else(|| fdo::Error::Failed(format!("{user} is not a managed user")))?;
        serde_json::to_string(&st).map_err(|e| fdo::Error::Failed(e.to_string()))
    }

    async fn add_bonus(
        &self,
        user: &str,
        secs: i64,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let sign = if secs < 0 { "-" } else { "+" };
        let what = format!("bonus {sign}{}", duration::format(secs.unsigned_abs()));
        self.admin(conn, &hdr, user, &what, |u| u.bonus_secs += secs)
            .await
    }

    /// No limits or windows for the rest of today (or back off with false).
    async fn set_unlimited(
        &self,
        user: &str,
        on: bool,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let what = if on {
            "limits off for today"
        } else {
            "limits back on"
        };
        self.admin(conn, &hdr, user, what, |u| {
            u.unlimited = on;
            if on {
                u.ended = false;
            }
        })
        .await
    }

    /// Allow use until "HH:MM" today, past the scheduled windows.
    async fn extend_until(
        &self,
        user: &str,
        until: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        let minute = parse_hhmm(until).map_err(|e| fdo::Error::InvalidArgs(format!("{e:#}")))?;
        let now = Local::now().naive_local().time();
        if minute <= chrono::Timelike::num_seconds_from_midnight(&now) / 60 {
            return Err(fdo::Error::InvalidArgs(format!(
                "{until} has already passed"
            )));
        }
        let what = format!("extended until {}", fmt_hhmm(minute));
        self.admin(conn, &hdr, user, &what, |u| {
            u.extend_until = Some(minute);
            u.ended = false;
        })
        .await
    }

    /// Out of time for today, right now.
    async fn end_today(
        &self,
        user: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        self.admin(conn, &hdr, user, "time ended for today", |u| {
            u.ended = true;
            u.unlimited = false;
        })
        .await
    }

    /// Drop today's off/extend/end overrides (bonus time stays).
    async fn clear_overrides(
        &self,
        user: &str,
        #[zbus(header)] hdr: Header<'_>,
        #[zbus(connection)] conn: &Connection,
    ) -> fdo::Result<String> {
        self.admin(conn, &hdr, user, "overrides cleared", |u| {
            u.unlimited = false;
            u.ended = false;
            u.extend_until = None;
        })
        .await
    }
}

/// Name -> uid from /etc/passwd.
fn lookup_uids(users: impl Iterator<Item = String>) -> Result<HashMap<String, u32>> {
    let passwd = std::fs::read_to_string("/etc/passwd").context("reading /etc/passwd")?;
    let mut out = HashMap::new();
    for user in users {
        let uid = passwd.lines().find_map(|line| {
            let mut f = line.split(':');
            (f.next() == Some(&user)).then(|| f.nth(1)?.parse().ok())?
        });
        match uid {
            Some(uid) => {
                out.insert(user, uid);
            }
            None => eprintln!("warning: managed user {user} not found in /etc/passwd"),
        }
    }
    Ok(out)
}

/// For each uid: does it have any session at all, and is one of them in the
/// foreground and unlocked?
async fn session_states(
    conn: &Connection,
    login: &LoginManagerProxy<'_>,
) -> Result<HashMap<u32, bool>> {
    let mut out = HashMap::new();
    for (_id, uid, _user, _seat, path) in login.list_sessions().await? {
        let session = LoginSessionProxy::builder(conn)
            .path(path)?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        // "user" sessions only: skip the greeter and the per-user manager.
        let class = session.class().await.unwrap_or_default();
        if !class.starts_with("user") {
            continue;
        }
        let foreground = session.active().await.unwrap_or(false)
            && !session.locked_hint().await.unwrap_or(false);
        *out.entry(uid).or_insert(false) |= foreground;
    }
    Ok(out)
}

async fn tick(
    shared: &Mutex<Shared>,
    conn: &Connection,
    login: &LoginManagerProxy<'_>,
    elapsed: Duration,
) -> Result<()> {
    let sessions = session_states(conn, login).await?;
    let mut to_terminate = Vec::new();
    {
        let mut guard = shared.lock().await;
        let sh = &mut *guard;
        let now = Local::now().naive_local();
        let users: Vec<(String, u32)> = sh.uids.iter().map(|(n, u)| (n.clone(), *u)).collect();
        for (user, uid) in users {
            let Some(&foreground) = sessions.get(&uid) else {
                sh.runtime.insert(user, Runtime::default());
                continue;
            };
            let hb_timeout = Duration::from_secs(sh.cfg.heartbeat_timeout_secs);
            let idle = sh
                .heartbeats
                .get(&uid)
                .is_some_and(|hb| hb.idle && hb.at.elapsed() < hb_timeout);
            let counting = foreground && !idle;
            if counting {
                sh.state.today(&user, now.date()).used_secs += elapsed.as_secs_f64().round() as u64;
                sh.dirty = true;
            }

            let usage = sh.state.peek(&user, now.date());
            let st = sh.cfg.users[&user].evaluate(&user, now, &usage);
            let deadline = Duration::from_secs(sh.cfg.logout_grace_secs + sh.cfg.force_logout_secs);
            let rt = sh.runtime.entry(user.clone()).or_default();
            rt.counting = counting;
            if st.allowed_now {
                rt.out_since = None;
            } else if rt.out_since.get_or_insert_with(Instant::now).elapsed() >= deadline {
                rt.out_since = None;
                to_terminate.push((user, uid));
            }
        }
    }
    for (user, uid) in to_terminate {
        eprintln!("{user}: still logged in after time ran out; terminating sessions");
        if let Err(e) = login.terminate_user(uid).await {
            eprintln!("{user}: terminate failed: {e}");
        }
    }
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    // `screentimed --check FILE`: validate a config (used at build time).
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let [flag, file] = args.as_slice()
        && flag == "--check"
    {
        let cfg = Config::load(file.as_ref())?;
        println!("{file}: ok ({} user(s))", cfg.users.len());
        return Ok(());
    }

    let cfg = Config::load(&config::path())?;
    let state_path = state::path();
    let state = State::load(&state_path)?;
    let uids = lookup_uids(cfg.users.keys().cloned())?;
    eprintln!(
        "managing {:?}; idle after {}s",
        uids.keys().collect::<Vec<_>>(),
        cfg.idle_timeout_secs
    );

    let shared = Arc::new(Mutex::new(Shared {
        cfg,
        uids,
        state,
        state_path,
        dirty: false,
        heartbeats: HashMap::new(),
        runtime: HashMap::new(),
    }));

    let builder = if std::env::var("SCREENTIME_BUS").as_deref() == Ok("session") {
        zbus::connection::Builder::session()?
    } else {
        zbus::connection::Builder::system()?
    };
    let conn = builder
        .name(BUS_NAME)?
        .serve_at(
            OBJECT_PATH,
            Iface {
                shared: shared.clone(),
            },
        )?
        .build()
        .await
        .context("connecting to the system bus")?;
    let login = LoginManagerProxy::new(&conn).await?;

    let mut interval = tokio::time::interval(TICK);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_tick = Instant::now();
    let mut last_save = Instant::now();
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    loop {
        tokio::select! {
            _ = interval.tick() => {}
            _ = term.recv() => break,
            _ = tokio::signal::ctrl_c() => break,
        }
        // Cap the step so a suspend/resume gap isn't counted.
        let elapsed = last_tick.elapsed().min(TICK * 3);
        last_tick = Instant::now();
        if let Err(e) = tick(&shared, &conn, &login, elapsed).await {
            eprintln!("tick: {e:#}");
        }
        let mut sh = shared.lock().await;
        if sh.dirty && last_save.elapsed() >= SAVE_EVERY {
            sh.save();
            last_save = Instant::now();
        }
    }

    shared.lock().await.save();
    Ok(())
}
