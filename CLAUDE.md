# CLAUDE.md

Notes for Claude Code sessions on this repo. README.md documents the design,
options, CLI and D-Bus interface; read it first. This file records the context
and hard-won lessons that aren't obvious from the code.

## Where this runs

- Built for one real machine: **the kid's PC** (Dell XPS 15 9560, NixOS unstable,
  KDE Plasma 6 on Wayland, SDDM). Managed user `kid` is a locked-down child account;
  the parent/admin account is SSH-only (wheel) and uses the `screentime` CLI over SSH.
- The NixOS config (the parent's separate `nixos-config` repo) pulls this repo as a
  non-flake input and imports the module:
  ```nix
  inputs.screentime = { url = "github:kmorris81/screentime"; flake = false; };
  # configuration.nix
  imports = [ "${inputs.screentime}/module.nix" ./screentime.nix ];
  ```
  So changes here reach the machine only after pushing to GitHub and running
  `nix flake update screentime` in the config repo. Keep `module.nix` and
  `package.nix` working without a flake (`callPackage ./package.nix {}`).
- The kid's account is heavily locked down by that config, which affects what
  this app can rely on:
  - polkit default-deny for `kid` (only power/suspend, `login1.inhibit-*`,
    `login1.chvt`, RealtimeKit allowed). Anything the agent does must not
    need polkit; privileged work belongs in `screentimed`.
  - The kid's home, /tmp, /var/tmp, /dev/shm and /run/user are mounted
    `noexec`. The agent runs from the Nix store, which is fine.
  - DNS is locked to a Pi-hole; the box may have no general internet.
- The schedule (`services.screentime.users.kid.days`) lives in the config
  repo's `screentime.nix`, not here.

## Lessons learned (don't regress these)

- **Black screen on time-up.** The first version ended sessions with logind
  `TerminateUser`, which killed Plasma and left a black screen instead of
  SDDM. Fix: the agent asks Plasma to log out cleanly via
  `org.kde.Shutdown.logout` (fallback `org.kde.ksmserver`), and the daemon's
  `TerminateUser` is only a last resort after `logout_grace_secs +
  force_logout_secs`. Keep that order. The same order applies to the
  `time_up_action` modes (default `shutdown`): agent asks Plasma
  (`logoutAndShutdown`, or ScreenSaver `Lock`), daemon falls back to logind
  `PowerOffWithFlags(SKIP_INHIBITORS)` (the kid may hold inhibitor locks) or
  `Session.Lock`.
- **Never shut down on a refused login.** The daemon only enforces for users
  with a session open when time ran out; a pending shutdown survives the
  session ending (Plasma logged out but didn't power off), nothing else does.
- **Screen locking is not idle detection.** The predecessor (timekpr-nExT)
  only noticed idleness once the screen locked, and locking on KDE was the
  original pain. We use Wayland `ext-idle-notify-v1`; v2's
  `get_input_idle_notification` ignores idle inhibitors (video, games) and is
  what `inputIdleOnly` relies on.
- **D-Bus method names are case-sensitive.** zbus exports Rust method names
  in PascalCase by default; Plasma's method is lowercase `logout`, so the fake
  in `examples/fake_logind.rs` needs `#[zbus(name = "logout")]`.
- **PAM rule placement.** The rule goes on the `login` service (SDDM's account
  stack includes `login` on NixOS) with `order = 10000` so it runs before any
  `sufficient` rule. Referencing another rule's order (e.g.
  `rules.account.unix`) broke evaluation because that rule doesn't exist on
  every service.
- **Fail closed.** The agent runs as the kid and is untrusted: missing
  heartbeats count as active, heartbeats are accepted only from the packaged
  agent binary (checked via `/proc/<pid>/exe`), and `pam-check` refuses on any
  error for a managed user. Preserve these properties.

## Dependencies

- All crates were latest at the time of writing, **except zbus, held at 5**
  because `ksni` 0.3.x depends on zbus 5. Don't bump zbus until ksni does.
- `rust-version = "1.89"` (edition 2024). Check it still matches the MSRV of
  the dependencies when bumping.
- The parent/admin cares that versions are current; when adding a crate, use the latest
  release, not a remembered one.

## Development

- `cargo build`, `cargo test`, `cargo clippy --all-targets`. `package.nix`
  runs the tests in its check phase.
- Releases: tag `vX.Y.Z` matching `Cargo.toml`; CI builds static musl
  binaries and commits `release.json`, which the module uses only when its
  version equals `Cargo.toml`'s. Bump the version with any code change:
  otherwise release.json still matches and the new `module.nix` installs the
  old binaries. (Bumped but not yet released builds from source.)
  The CI commit lands on master; pull before pushing again.
- End-to-end testing without a desktop: see "End-to-end testing" in README.md
  (`examples/fake_logind.rs` on a private session bus with
  `SCREENTIME_BUS=session`).
- Real-world testing happens on the kid's PC: rebuild the config there, then
  `journalctl -u screentimed -f` and
  `sudo journalctl _SYSTEMD_USER_UNIT=screentime-agent.service`.
- The dev container used before had no nix or Rust by default and was reset
  often; reinstall as needed (`apt-get install nix-bin`, rustup). Unsandboxed
  nix builds there leave `/homeless-shelter` behind; delete it between builds.
