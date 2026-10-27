//! screentime: admin CLI, and the PAM login check.
//!
//!   screentime status [USER]           time left / used (default: everyone)
//!   sudo screentime bonus USER 30m     add time for today (negative removes)
//!   sudo screentime off|on|end USER    today-only overrides
//!   sudo screentime extend USER 21:30  allow past the window until 21:30
//!   screentime pam-check               for pam_exec: refuse logins with no time

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use chrono::Local;

use screentime::config::{self, Config};
use screentime::dbus::{DaemonProxy, daemon_bus};
use screentime::state::{self, State};
use screentime::{Status, duration};

const USAGE: &str = "\
usage:
  screentime status [USER]          show time left and used today
  screentime bonus USER DURATION    add time today, e.g. 30m, 1h, -15m
  screentime off USER               no limits or windows for the rest of today
  screentime on USER                undo off/extend/end for today (bonus stays)
  screentime extend USER HH:MM      allow use past today's window until HH:MM
  screentime end USER               out of time now (time-up action after grace)
  screentime pam-check              PAM account check (uses $PAM_USER)

Everything except status needs root. Overrides reset at midnight.";

fn print_status(st: &Status) {
    let state = if st.ended {
        "ended for today"
    } else if !st.allowed_now {
        "locked out"
    } else if st.unlimited {
        "no limits today"
    } else if st.counting {
        "counting"
    } else {
        "paused"
    };
    println!("{} ({state})", st.user);
    println!("  left:   {}", duration::format(st.remaining_secs));
    println!(
        "  used:   {} of {}",
        duration::format(st.used_secs),
        duration::format(st.limit_secs)
    );
    if st.bonus_secs != 0 {
        let sign = if st.bonus_secs < 0 { "-" } else { "+" };
        println!(
            "  bonus:  {sign}{}",
            duration::format(st.bonus_secs.unsigned_abs())
        );
    }
    if let Some(end) = &st.window_end {
        println!("  window: until {end}");
    }
    if let Some(until) = &st.extended_until {
        println!("  extended until {until}");
    }
    if let Some(next) = &st.next_window {
        println!("  next:   {next}");
    }
}

async fn status(user: Option<&str>) -> Result<()> {
    let conn = daemon_bus().await?;
    let daemon = DaemonProxy::new(&conn).await?;
    let users: Vec<String> = match user {
        Some(u) => vec![u.to_string()],
        // No user given: everyone in the config (falls back to the caller).
        None => match Config::load(&config::path()) {
            Ok(cfg) if !cfg.users.is_empty() => cfg.users.keys().cloned().collect(),
            _ => vec![String::new()],
        },
    };
    for user in users {
        let json = daemon
            .status(&user)
            .await
            .with_context(|| format!("asking screentimed about {user:?}"))?;
        print_status(&serde_json::from_str(&json)?);
    }
    Ok(())
}

/// An admin change to today: one of the root-only daemon methods.
enum Change<'a> {
    Bonus(i64),
    Off,
    Extend(&'a str),
    End,
    Clear,
}

async fn change(user: &str, change: Change<'_>) -> Result<()> {
    let conn = daemon_bus().await?;
    let daemon = DaemonProxy::new(&conn).await?;
    let json = match change {
        Change::Bonus(secs) => daemon.add_bonus(user, secs).await,
        Change::Off => daemon.set_unlimited(user, true).await,
        Change::Extend(until) => daemon.extend_until(user, until).await,
        Change::End => daemon.end_today(user).await,
        Change::Clear => daemon.clear_overrides(user).await,
    }
    // zbus prints "org.freedesktop.DBus.Error.AccessDenied: ..."; keep the message.
    .map_err(|e| match e {
        zbus::Error::MethodError(_, Some(msg), _) => anyhow::anyhow!(msg),
        e => e.into(),
    })?;
    print_status(&serde_json::from_str(&json)?);
    Ok(())
}

/// Exit 0 to allow the login, 1 to refuse it. Works from the config and state
/// files directly, so it doesn't depend on the daemon being up. Unmanaged
/// users always pass; for managed users, any error refuses (fail closed).
fn pam_check() -> ExitCode {
    let Ok(user) = std::env::var("PAM_USER") else {
        eprintln!("screentime pam-check: PAM_USER not set");
        return ExitCode::SUCCESS;
    };
    let cfg = match Config::load(&config::path()) {
        Ok(cfg) => cfg,
        Err(e) => {
            // Can't tell whether this user is managed; only refuse if the
            // raw file mentions them.
            eprintln!("screentime pam-check: {e:#}");
            let raw = std::fs::read_to_string(config::path()).unwrap_or_default();
            let managed = raw.contains(&format!("users.{user}."));
            return if managed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            };
        }
    };
    let Some(schedule) = cfg.users.get(&user) else {
        return ExitCode::SUCCESS;
    };
    let state = match State::load(&state::path()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("screentime pam-check: {e:#}");
            return ExitCode::FAILURE;
        }
    };
    let now = Local::now().naive_local();
    let usage = state.peek(&user, now.date());
    let st = schedule.evaluate(&user, now, &usage);
    if st.allowed_now {
        return ExitCode::SUCCESS;
    }
    // pam_exec's `stdout` option shows this to the user.
    match &st.next_window {
        Some(next) => println!("No screen time left right now. Next: {next}."),
        None => println!("No screen time left right now."),
    }
    ExitCode::FAILURE
}

async fn run(args: &[String]) -> Result<()> {
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["status"] => status(None).await,
        ["status", user] => status(Some(user)).await,
        ["bonus", user, amount] => change(user, Change::Bonus(duration::parse(amount)?)).await,
        ["off", user] => change(user, Change::Off).await,
        ["on", user] => change(user, Change::Clear).await,
        ["extend", user, until] => change(user, Change::Extend(until)).await,
        ["end", user] => change(user, Change::End).await,
        ["help" | "-h" | "--help"] => {
            println!("{USAGE}");
            Ok(())
        }
        _ => bail!("{USAGE}"),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("pam-check") {
        return pam_check();
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    match rt.block_on(run(&args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e:#}");
            ExitCode::FAILURE
        }
    }
}
