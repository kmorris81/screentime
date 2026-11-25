//! screentime: admin CLI, and the PAM login check.
//!
//!   screentime status [--json] [USER]  time left / used (default: everyone)
//!   sudo screentime bonus USER 30m     add time for today (negative removes)
//!   sudo screentime off|on|end USER    today-only overrides
//!   sudo screentime allow USER 13:00-21:30  an extra window today
//!   screentime pam-check               for pam_exec: refuse logins with no time
//!   screentime --server URL ...        the same against screentime-server,
//!                                      plus `config get|set|edit`

use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Local;

use screentime::config::{self, Config};
use screentime::dbus::{DaemonProxy, daemon_bus};
use screentime::remote::{self, RemoteStatus};
use screentime::schedule::{fmt_window, parse_window};
use screentime::state::{self, Change, State};
use screentime::{Status, duration};

const USAGE: &str = "\
usage:
  screentime [--server URL] COMMAND

  status [--json] [USER]  show time left and used today
                          (--json: one JSON object per line)
  bonus USER DURATION     add time today, e.g. 30m, 1h, -15m
  off USER                no limits or windows for the rest of today
  on USER                 undo off/allow/end for today (bonus stays)
  allow USER HH:MM-HH:MM  an extra window today, e.g. to start early, or
                          both early and late (merged with the schedule)
  end USER                out of time now (limit action after grace)
  pam-check               PAM account check (uses $PAM_USER)

With a server (--server URL or $SCREENTIME_SERVER, e.g. pi.lan:8470):
  config get              print the server's policy (config TOML)
  config set FILE         replace it (checked first; - reads stdin)
  config edit             edit it in $EDITOR

Without --server, changes go to the local daemon and need root, unless this
PC is in server mode: then they go to its server. Overrides reset at midnight.";

const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

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
    if !st.extra_windows.is_empty() {
        println!("  extra:  {} (today)", st.extra_windows.join(", "));
    }
    if let Some(next) = &st.next_window {
        println!("  next:   {next}");
    }
}

fn print_remote(rs: &RemoteStatus) {
    print_status(&rs.status);
    match (&rs.host, &rs.last_seen) {
        (Some(host), Some(at)) => {
            let when = if at.date_naive() == Local::now().date_naive() {
                at.format("%H:%M")
            } else {
                at.format("%a %d %b %H:%M")
            };
            println!("  seen:   {host} at {when}");
        }
        _ => println!("  seen:   never (no PC has reported this user)"),
    }
}

async fn status(user: Option<&str>, json: bool) -> Result<()> {
    let conn = daemon_bus().await?;
    let daemon = DaemonProxy::new(&conn).await?;
    let users: Vec<String> = match user {
        Some(u) => vec![u.to_string()],
        // No user given: everyone in the config (falls back to the caller).
        None => match config::load() {
            Ok(cfg) if !cfg.users.is_empty() => cfg.users.keys().cloned().collect(),
            _ => vec![String::new()],
        },
    };
    for user in users {
        let reply = daemon
            .status(&user)
            .await
            .with_context(|| format!("asking screentimed about {user:?}"))?;
        let st: Status = serde_json::from_str(&reply)?;
        if json {
            println!("{}", serde_json::to_string(&st)?);
        } else {
            print_status(&st);
        }
    }
    Ok(())
}

async fn change(user: &str, change: Change) -> Result<()> {
    let conn = daemon_bus().await?;
    let daemon = DaemonProxy::new(&conn).await?;
    let json = match change {
        Change::Bonus(secs) => daemon.add_bonus(user, secs).await,
        Change::Off => daemon.set_unlimited(user, true).await,
        Change::Allow(w) => daemon.allow_window(user, &fmt_window(w)).await,
        Change::End => daemon.end_today(user).await,
        Change::Clear => daemon.clear_overrides(user).await,
    }
    // zbus prints "org.freedesktop.DBus.Error.AccessDenied: ..."; keep the message.
    .map_err(|e| match e {
        zbus::Error::MethodError(_, Some(msg), _) => anyhow::anyhow!(msg),
        e => e.into(),
    })?;
    let st: Status = serde_json::from_str(&json)?;
    print_status(&st);
    hint_no_minutes(&st, change);
    Ok(())
}

/// An extra window doesn't add minutes: say so when that leaves
/// none, e.g. on a day without windows, whose limit is 0.
fn hint_no_minutes(st: &Status, change: Change) {
    if matches!(change, Change::Allow(_)) && st.used_secs >= st.limit_secs {
        eprintln!(
            "note: no minutes left today; add some with `screentime bonus {} 1h`",
            st.user
        );
    }
}

/// Exit 0 to allow the login, 1 to refuse it. Works from the config and state
/// files directly, so it doesn't depend on the daemon being up. Unmanaged
/// users always pass; for managed users, any error refuses (fail closed).
fn pam_check() -> ExitCode {
    let Ok(user) = std::env::var("PAM_USER") else {
        eprintln!("screentime pam-check: PAM_USER not set");
        return ExitCode::SUCCESS;
    };
    let cfg = match config::load() {
        Ok(cfg) => cfg,
        Err(e) => {
            // Can't tell whether this user is managed; only refuse if the
            // raw file mentions them.
            eprintln!("screentime pam-check: {e:#}");
            let raw = std::fs::read_to_string(config::path()).unwrap_or_default();
            let managed =
                raw.contains(&format!("users.{user}.")) || raw.contains(&format!("users.{user}]"));
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

/// The server the CLI talks to, if any: --server, $SCREENTIME_SERVER, or
/// (`local_too`) this PC's own server in server mode.
fn server(flag: Option<&str>, local_too: bool) -> Option<String> {
    flag.map(str::to_string)
        .or_else(|| {
            std::env::var("SCREENTIME_SERVER")
                .ok()
                .filter(|s| !s.is_empty())
        })
        .or_else(|| {
            local_too
                .then(|| Config::load(&config::path()).ok()?.server_url)
                .flatten()
        })
}

fn remote_status(client: &remote::Client, user: Option<&str>, json: bool) -> Result<()> {
    let all: Vec<RemoteStatus> = match user {
        Some(u) => vec![client.get_json(&format!("/api/v1/status/{u}"))?],
        None => client.get_json("/api/v1/status")?,
    };
    if all.is_empty() {
        println!("the server's policy has no users yet (screentime config set FILE)");
    }
    for rs in all {
        if json {
            println!("{}", serde_json::to_string(&rs)?);
        } else {
            print_remote(&rs);
        }
    }
    Ok(())
}

fn remote_change(client: &remote::Client, user: &str, change: Change) -> Result<()> {
    let rs: RemoteStatus = client.post_json(&format!("/api/v1/users/{user}/change"), &change)?;
    print_remote(&rs);
    hint_no_minutes(&rs.status, change);
    println!(
        "(sent to {}; the PC applies it within a minute)",
        client.base()
    );
    Ok(())
}

fn config_set(client: &remote::Client, file: &str) -> Result<()> {
    let text = if file == "-" {
        std::io::read_to_string(std::io::stdin())?
    } else {
        std::fs::read_to_string(file).with_context(|| format!("reading {file}"))?
    };
    println!("{}", client.put_text("/api/v1/config", &text)?);
    Ok(())
}

/// Edit the policy in $EDITOR; on a rejected edit, offer to fix it.
fn config_edit(client: &remote::Client) -> Result<()> {
    let original = client.get_text("/api/v1/config")?;
    let path = std::env::temp_dir().join(format!("screentime-policy-{}.toml", std::process::id()));
    std::fs::write(&path, &original)?;
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());
    let result = loop {
        let ok = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{editor} \"$1\""))
            .arg("sh")
            .arg(&path)
            .status()
            .with_context(|| format!("running {editor}"))?
            .success();
        if !ok {
            break Err(anyhow::anyhow!("{editor} failed; nothing changed"));
        }
        let text = std::fs::read_to_string(&path)?;
        if text == original {
            println!("no changes");
            break Ok(());
        }
        match client.put_text("/api/v1/config", &text) {
            Ok(msg) => {
                println!("{msg}");
                break Ok(());
            }
            Err(e) => {
                eprintln!("rejected: {e:#}");
                eprint!("edit again? [Y/n] ");
                let mut answer = String::new();
                std::io::stdin().read_line(&mut answer)?;
                if answer.trim().eq_ignore_ascii_case("n") {
                    break Err(anyhow::anyhow!("nothing changed"));
                }
            }
        }
    };
    let _ = std::fs::remove_file(&path);
    result
}

fn parse_change(cmd: &str, rest: &[&str]) -> Result<Option<(String, Change)>> {
    let change = match (cmd, rest) {
        ("bonus", [user, amount]) => (user, Change::Bonus(duration::parse(amount)?)),
        ("off", [user]) => (user, Change::Off),
        ("on", [user]) => (user, Change::Clear),
        ("allow", [user, window]) => (user, Change::Allow(parse_window(window)?)),
        ("end", [user]) => (user, Change::End),
        _ => return Ok(None),
    };
    Ok(Some((change.0.to_string(), change.1)))
}

async fn run(args: &[String]) -> Result<()> {
    let mut args: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut flag = None;
    if let Some(i) = args.iter().position(|a| *a == "--server") {
        if i + 1 >= args.len() {
            bail!("--server needs a URL\n\n{USAGE}");
        }
        flag = Some(args[i + 1]);
        args.drain(i..=i + 1);
    }
    let client =
        |local_too| server(flag, local_too).map(|url| remote::Client::new(&url, HTTP_TIMEOUT));
    let Some((cmd, rest)) = args.split_first() else {
        bail!("{USAGE}");
    };
    match (*cmd, rest) {
        ("help" | "-h" | "--help", []) => {
            println!("{USAGE}");
            return Ok(());
        }
        ("status", _) => {
            let (json, user) = match rest {
                [] => (false, None),
                ["--json"] => (true, None),
                ["--json", user] | [user, "--json"] => (true, Some(*user)),
                [user] => (false, Some(*user)),
                _ => bail!("{USAGE}"),
            };
            // On a server-mode PC the local daemon knows best.
            return match client(false) {
                Some(c) => remote_status(&c, user, json),
                None => status(user, json).await,
            };
        }
        ("config", _) => {
            let c = client(true).context("config needs a server: --server URL")?;
            return match rest {
                ["get"] => {
                    print!("{}", c.get_text("/api/v1/config")?);
                    Ok(())
                }
                ["set", file] => config_set(&c, file),
                ["edit"] => config_edit(&c),
                _ => bail!("{USAGE}"),
            };
        }
        _ => {}
    }
    let Some((user, chg)) = parse_change(cmd, rest)? else {
        bail!("{USAGE}");
    };
    match client(true) {
        Some(c) => remote_change(&c, &user, chg),
        None => change(&user, chg).await,
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
