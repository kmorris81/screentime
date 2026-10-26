# screentime

A screen-time enforcement and management application for Linux/KDE/Wayland.

screentime limits when and how long a user (typically a kid's account) can use
the computer. You set allowed time windows and a daily limit for each day of
the week. Time only counts while the user is actually active, and once it's
used up they're logged out and can't log back in until their next window.
Everything is configured declaratively through a NixOS module, and an admin
CLI handles the day-to-day adjustments ("30 more minutes", "no limits today").

It was written as a replacement for timekpr-nExT, built specifically for KDE
Plasma on Wayland:

- **Idle detection without screen locking.** timekpr-nExT can only tell a
  user is idle once the screen is locked. screentime asks the compositor
  directly, through the Wayland `ext-idle-notify-v1` protocol.
- **A native tray icon.** The countdown is a StatusNotifierItem badge, not a
  GTK tray app.
- **Refusal at the login screen.** A user who is out of time can't log in at
  all, instead of logging in and getting kicked out.
- **A clean logout.** Time running out triggers a normal Plasma logout, so
  SDDM comes back properly instead of leaving a black screen.

## Features

- **Per-day schedule:** one or more allowed windows (e.g. `09:00-12:00`,
  `14:00-21:00`) plus a daily limit in minutes. Days left out of the schedule
  aren't allowed.
- **Activity-based counting:** time only counts while the user has a
  foreground, unlocked session and has touched the keyboard or mouse within
  the idle timeout (5 minutes by default). By default a playing video doesn't
  count as activity. Set `inputIdleOnly = false` to change that.
- **Tray countdown:** a badge showing the minutes left ("45", or "2h" above an
  hour). It is green, then amber below 15 minutes, then red below 5. The
  tooltip and menu show used/limit, when the current window ends, and when
  the next window starts.
- **Warnings:** desktop notifications at 10, 5 and 1 minutes left, and when
  time is up.
- **Enforcement:**
  - After a grace period (30 s by default) the session is logged out through
    Plasma, the same way as the Log Out button.
  - If the user is still logged in shortly after that, the daemon terminates
    their sessions through logind.
  - Logins are refused at SDDM and on text consoles until the next window.
- **Admin overrides for today:** bonus time, no limits for the rest of the
  day, extending past a window, or ending the day now. Overrides reset at
  midnight and survive restarts.
- **Build-time validation:** a typo or overlapping windows in the schedule
  fail `nixos-rebuild` instead of breaking the daemon at runtime.

## How it works

```
 ┌──────────── user's Plasma session ────────────┐
 │  screentime-agent (systemd user service)      │
 │   • Wayland ext-idle-notify  → idle / active  │
 │   • tray icon + notifications                 │
 │   • clean logout via org.kde.Shutdown         │
 └───────────────┬───────────────────────────────┘
                 │ system D-Bus: Heartbeat(idle), Status()
 ┌───────────────▼───────────────────────────────┐
 │  screentimed (root system service)            │
 │   • counts time, persists /var/lib/screentime │
 │   • asks logind which sessions are in front   │
 │   • terminates sessions as a last resort      │
 └───────────────▲───────────────────────────────┘
                 │ system D-Bus: bonus / off / extend / end
         screentime CLI (admin, e.g. over SSH)

  pam_exec → `screentime pam-check` refuses logins with no time left
```

| Component | Runs as | Job |
|---|---|---|
| `screentimed` | root, system service | The only source of truth. Every 5 s it asks logind which managed users have an active, unlocked session. It counts their time unless the agent says they're idle. It persists usage to `/var/lib/screentime/state.json` and ends sessions that are out of time. |
| `screentime-agent` | the managed user, systemd user service | Detects input idleness through Wayland, sends a heartbeat every 10 s, draws the tray icon, sends warnings, and requests the Plasma logout when time is up. |
| `screentime` | anyone (most commands need root) | The admin CLI, plus the `pam-check` used by PAM at login. |

### Tamper resistance

The agent runs as the restricted user, so it is treated as untrusted:

- **Fail-closed.** If the daemon hears no heartbeat for 60 s, it counts the
  user as active. Killing or stopping the agent can only cost the user time.
- **Heartbeats are authenticated.** The daemon asks the bus for the sender's
  PID and accepts a heartbeat only if `/proc/<pid>/exe` is the packaged agent
  binary. Something like `busctl call ... Heartbeat b true` is rejected.
- **Admin calls need root.** Status is readable by anyone. Every change
  (bonus, off, extend, end) requires uid 0.
- **The login check doesn't depend on the daemon.** `pam-check` reads the
  config and state files directly. For a managed user, any error refuses the
  login.
- **Suspend isn't counted.** Time is measured with a monotonic clock in capped
  steps, so a suspend/resume gap is never counted. The day boundary uses the
  system clock, so make sure the user can't change the system time; for
  example, deny `org.freedesktop.timedate1` through polkit.

screentime only manages time. It assumes the account is otherwise locked down:
no sudo, no way to change system services or the system clock.

## Requirements

- NixOS. The module and package are Nix; the binaries themselves are ordinary
  Rust programs.
- KDE Plasma 6 on Wayland, with SDDM.
  - **Idle detection:** needs a compositor that supports `ext-idle-notify-v1`.
    KWin does. Version 2 of the protocol, which ignores idle inhibitors, is
    needed for `inputIdleOnly`. With only version 1, apps that inhibit idle
    keep the counter running.
  - **Tray:** works with any StatusNotifierItem host.
  - **Clean logout:** Plasma-specific (`org.kde.Shutdown`). On other desktops
    the daemon's forced logout still applies.
- systemd-logind.

## Installation (NixOS)

Import the module and describe the schedule:

```nix
{
  imports = [ ./screentime/module.nix ];

  services.screentime = {
    enable = true;
    idleMinutes = 5;

    users.kid.days = {
      mon = { windows = [ "15:00-20:00" ]; limitMinutes = 90; };
      tue = { windows = [ "15:00-20:00" ]; limitMinutes = 90; };
      wed = { windows = [ "15:00-20:00" ]; limitMinutes = 90; };
      thu = { windows = [ "15:00-20:00" ]; limitMinutes = 90; };
      fri = { windows = [ "15:00-21:00" ]; limitMinutes = 120; };
      sat = { windows = [ "09:00-12:00" "14:00-21:00" ]; limitMinutes = 180; };
      sun = { windows = [ "09:00-12:00" "14:00-20:00" ]; limitMinutes = 180; };
    };
  };
}
```

If this repository is a separate checkout, fetch it and import the module
from the fetched path. If your system configuration is a flake kept in git,
the module's files must be visible to that flake (for example, vendored or
added as a submodule).

Rebuild, then check `systemctl status screentimed`. The agent starts with the
managed user's next graphical login.

### Options

All options live under `services.screentime`.

| Option | Default | Description |
|---|---|---|
| `enable` | `false` | Enable screentime. |
| `users.<name>.days.<day>.windows` | `[ ]` | Allowed windows for `mon`..`sun`, as `"HH:MM-HH:MM"`. The end is exclusive and `24:00` is allowed. A window can't cross midnight; split it in two instead. Windows on the same day can't overlap. |
| `users.<name>.days.<day>.limitMinutes` | `0` | Screen time allowed that day. |
| `idleMinutes` | `5` | Minutes without keyboard/mouse input after which the counter stops. |
| `inputIdleOnly` | `true` | Only real input counts as activity; idle inhibitors (video, games) are ignored. |
| `logoutGraceSeconds` | `30` | Time between running out and the clean Plasma logout. |
| `forceLogoutSeconds` | `20` | If the user is still logged in this long after the grace period, terminate their sessions through logind. |
| `pamServices` | `[ "login" ]` | PAM services that refuse logins without time left. On NixOS, SDDM's account stack includes `login`, so this covers both graphical and console logins. |
| `package` | built from this repo | The screentime package. |

The module:
- writes `/etc/screentime/config.toml`, validated at build time with
  `screentimed --check`
- installs the D-Bus policy
- adds the `screentimed` system service and the `screentime-agent` user
  service, the latter only for managed users
- adds a `pam_exec` account rule

## Admin CLI

```sh
screentime status [USER]            # time left, used/limit, bonus, window, next window
sudo screentime bonus USER 30m      # add time today: 30, 30m, 1h, 1h30m, 90s, -15m
sudo screentime off USER            # no limits or windows for the rest of today
sudo screentime extend USER 21:30   # allow use past today's window until 21:30
sudo screentime end USER            # out of time now (logged out after the grace period)
sudo screentime on USER             # undo off/extend/end for today (bonus stays)
screentime help
```

Example:

```
$ screentime status kid
kid (counting)
  left:   47m
  used:   43m of 1h 30m
  window: until 20:00
  next:   Sat 09:00
```

How the overrides interact:
- `extend` stretches today's allowed time from the start of the first window
  to the given time. It also fills any gap between windows. The daily limit
  still applies, so combine it with `bonus` if the minutes are used up too.
- `end` wins over everything else.
- `on` clears `off`, `extend` and `end`, but keeps any bonus.

Logs:

```sh
journalctl -u screentimed -f                                  # counting, overrides, logouts
sudo journalctl _SYSTEMD_USER_UNIT=screentime-agent.service   # agent (idle, tray, logout)
```

## Files and interfaces

| Path | Purpose |
|---|---|
| `/etc/screentime/config.toml` | Generated config (path overridable with `$SCREENTIME_CONFIG`). |
| `/var/lib/screentime/state.json` | Today's usage and overrides per user (overridable with `$SCREENTIME_STATE_DIR`). |
| `share/dbus-1/system.d/org.screentime.Daemon1.conf` | Bus policy: only root may own the name; anyone may call it. |

D-Bus interface `org.screentime.Daemon1` at `/org/screentime/Daemon1` on the
system bus:

| Method | Caller | Description |
|---|---|---|
| `Heartbeat(b idle)` | agent binary only | Liveness and idle state. |
| `Status(s user) → s` | anyone | JSON status. An empty user means the caller. |
| `AddBonus(s user, x secs) → s` | root | Add or remove time for today. |
| `SetUnlimited(s user, b on) → s` | root | No limits for the rest of today. |
| `ExtendUntil(s user, s "HH:MM") → s` | root | Allow use until that time today. |
| `EndToday(s user) → s` | root | No more time today. |
| `ClearOverrides(s user) → s` | root | Drop today's off/extend/end. |

## Development

```sh
cargo build
cargo test
cargo clippy --all-targets
```

The code is laid out as:
- **Library:** `src/` holds the config parsing, schedule evaluation, state
  persistence, D-Bus proxies and tray icon rendering.
- **Binaries:** the three programs live in `src/bin/`.
- **Packaging:** `package.nix` builds the package (running the tests in its
  check phase), and `module.nix` is the NixOS module.

### End-to-end testing without a desktop

`examples/fake_logind.rs` stands in for logind, and optionally for Plasma's
`org.kde.Shutdown`, on a private bus. Every program accepts
`SCREENTIME_BUS=session`, so the whole flow can run without root or a real
session:

```sh
cargo build --bins --examples
cat > /tmp/st.toml <<EOF
logout_grace_secs = 5
agent_path = "$PWD/target/debug/screentime-agent"
[users.$USER.days.$(date +%a | tr A-Z a-z)]
windows = ["00:00-24:00"]
limit_minutes = 1
EOF
dbus-run-session -- sh -c '
  export SCREENTIME_BUS=session SCREENTIME_CONFIG=/tmp/st.toml SCREENTIME_STATE_DIR=/tmp
  FAKE_UID=$(id -u) FAKE_KDE=1 target/debug/examples/fake_logind &
  sleep 1; target/debug/screentimed &
  sleep 1; target/debug/screentime-agent &
  sleep 2; target/debug/screentime status
  sleep 90'
```

On that bus, `AddBonus` and the other admin calls still require uid 0.

The fake supports these environment variables:
- `FAKE_UID`: the session owner.
- `FAKE_LOCKED`: report the session as locked.
- `FAKE_KEEP`: keep the session after `TerminateUser`, to watch repeated
  enforcement.
- `FAKE_KDE`: also serve `org.kde.Shutdown.logout`.

## Limitations

- Only KDE Plasma on Wayland has been targeted. Idle detection and the tray
  should work on other Wayland compositors and StatusNotifierItem hosts, but
  the clean logout won't.
- Windows can't cross midnight; split them in two instead.
- There are no weekly or monthly limits.
- An app with unsaved changes can cancel Plasma's logout. In that case the
  daemon's forced logout follows after `forceLogoutSeconds`.
- The SDDM greeter may show only a generic "login failed" rather than
  screentime's explanation.
