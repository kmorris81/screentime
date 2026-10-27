//! screentimed: system daemon that counts and enforces screen time.
//!
//! Every tick it asks logind which managed users have a foreground, unlocked
//! session. Time is counted unless that user's agent reports them idle. With
//! no (or a stale) heartbeat the user counts as active, so killing the agent
//! never buys extra time. Out of time or outside a window, the agent logs the
//! user out, shuts down or locks the screen through Plasma after a grace
//! period (`limit_action` / `window_end_action`); if that hasn't happened a
//! bit later, the daemon does it through logind.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use tokio::sync::Mutex;
use zbus::message::Header;
use zbus::proxy::CacheProperties;
use zbus::zvariant::OwnedObjectPath;
use zbus::{Connection, fdo};

use screentime::config::{self, Config, TimeUpAction};
use screentime::dbus::{LoginManagerProxy, LoginSessionProxy, SKIP_INHIBITORS};
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
    /// When the user ran out of time (or left the window) with sessions
    /// open, and the action that calls for. Cleared once enforced, except
    /// for `lock`, which repeats.
    out: Option<(Instant, TimeUpAction)>,
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

/// One managed user's sessions, as seen by logind.
#[derive(Default)]
struct UserSessions {
    /// One of them is in the foreground and unlocked.
    foreground: bool,
    /// Sessions that aren't locked.
    unlocked: Vec<OwnedObjectPath>,
}

/// Sessions per uid; a uid with no sessions is missing.
async fn session_states(
    conn: &Connection,
    login: &LoginManagerProxy<'_>,
) -> Result<HashMap<u32, UserSessions>> {
    let mut out: HashMap<u32, UserSessions> = HashMap::new();
    for (_id, uid, _user, _seat, path) in login.list_sessions().await? {
        let session = LoginSessionProxy::builder(conn)
            .path(path.clone())?
            .cache_properties(CacheProperties::No)
            .build()
            .await?;
        // "user" sessions only: skip the greeter and the per-user manager.
        let class = session.class().await.unwrap_or_default();
        if !class.starts_with("user") {
            continue;
        }
        let locked = session.locked_hint().await.unwrap_or(false);
        let entry = out.entry(uid).or_default();
        entry.foreground |= session.active().await.unwrap_or(false) && !locked;
        if !locked {
            entry.unlocked.push(path);
        }
    }
    Ok(out)
}

/// Last-resort enforcement once the agent's clean action hasn't worked.
enum Enforce {
    Terminate(String, u32),
    Lock(String, Vec<OwnedObjectPath>),
    PowerOff(String),
}

async fn tick(
    shared: &Mutex<Shared>,
    conn: &Connection,
    login: &LoginManagerProxy<'_>,
    elapsed: Duration,
) -> Result<()> {
    let mut sessions = session_states(conn, login).await?;
    let mut enforce = Vec::new();
    {
        let mut guard = shared.lock().await;
        let sh = &mut *guard;
        let now = Local::now().naive_local();
        let users: Vec<(String, u32)> = sh.uids.iter().map(|(n, u)| (n.clone(), *u)).collect();
        for (user, uid) in users {
            let rt = sh.runtime.entry(user.clone()).or_default();
            let user_sessions = sessions.remove(&uid);
            // Only a user with sessions open can trigger enforcement, so a
            // refused login never does. The exception: a shutdown that
            // started while logged in must still happen if Plasma ended the
            // session but didn't manage to power off.
            let shutting_down = matches!(rt.out, Some((_, TimeUpAction::Shutdown)));
            if user_sessions.is_none() && !shutting_down {
                *rt = Runtime::default();
                continue;
            }
            let UserSessions {
                foreground,
                unlocked,
            } = user_sessions.unwrap_or_default();
            let hb_timeout = Duration::from_secs(sh.cfg.heartbeat_timeout_secs);
            let idle = sh
                .heartbeats
                .get(&uid)
                .is_some_and(|hb| hb.idle && hb.at.elapsed() < hb_timeout);
            let counting = foreground && !idle;
            rt.counting = counting;
            if counting {
                sh.state.today(&user, now.date()).used_secs += elapsed.as_secs_f64().round() as u64;
                sh.dirty = true;
            }

            let usage = sh.state.peek(&user, now.date());
            let st = sh.cfg.users[&user].evaluate(&user, now, &usage);
            let deadline = Duration::from_secs(sh.cfg.logout_grace_secs + sh.cfg.force_logout_secs);
            let Some(cutoff) = st.cutoff else {
                rt.out = None;
                continue;
            };
            let action = sh.cfg.action(cutoff);
            // A different action (say the window ends while the screen is
            // locked for the limit) starts over with its own grace period,
            // as the agent does.
            let since = match rt.out {
                Some((since, pending)) if pending == action => since,
                _ => rt.out.insert((Instant::now(), action)).0,
            };
            if since.elapsed() < deadline {
                continue;
            }
            match action {
                TimeUpAction::Logout => {
                    rt.out = None;
                    enforce.push(Enforce::Terminate(user, uid));
                }
                // Keep `out`: if the user unlocks again, lock again on
                // the next tick.
                TimeUpAction::Lock if !unlocked.is_empty() => {
                    enforce.push(Enforce::Lock(user, unlocked));
                }
                TimeUpAction::Lock => {}
                TimeUpAction::Shutdown => {
                    rt.out = None;
                    enforce.push(Enforce::PowerOff(user));
                }
            }
        }
    }
    for e in enforce {
        match e {
            Enforce::Terminate(user, uid) => {
                eprintln!("{user}: still logged in after time ran out; terminating sessions");
                if let Err(e) = login.terminate_user(uid).await {
                    eprintln!("{user}: terminate failed: {e}");
                }
            }
            Enforce::Lock(user, paths) => {
                eprintln!("{user}: unlocked after time ran out; locking");
                for path in paths {
                    let locked = async {
                        LoginSessionProxy::builder(conn)
                            .path(path)?
                            .build()
                            .await?
                            .lock()
                            .await
                    };
                    if let Err(e) = locked.await {
                        eprintln!("{user}: lock failed: {e}");
                    }
                }
            }
            Enforce::PowerOff(user) => {
                eprintln!("{user}: time ran out and the machine is still up; powering off");
                if let Err(e) = login.power_off_with_flags(SKIP_INHIBITORS).await {
                    eprintln!("PowerOffWithFlags failed ({e}); trying PowerOff");
                    if let Err(e) = login.power_off(false).await {
                        eprintln!("power off failed: {e}");
                    }
                }
                break;
            }
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
