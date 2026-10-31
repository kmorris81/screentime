# CLAUDE.md

Notes for Claude Code sessions on this repo. README.md documents the design,
options, CLI and D-Bus interface; read it first. This file records the context
and hard-won lessons that aren't obvious from the code.

## Where this runs

- Built for one real machine: **the kid's PC** (Dell XPS 15 9560, NixOS unstable,
  KDE Plasma 6 on Wayland, SDDM). Managed user `kid` is a locked-down child account;
  the parent/admin account is SSH-only (wheel) and uses the `screentime` CLI over SSH.
  There are now two such PCs, one per kid (`benny`, `eli`), each with a single
  managed user named after the kid; options that pick "the" user (e.g.
  `loginScreen.user`) default to it when there's only one.
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
  force_logout_secs`. Keep that order. The same order applies to every
  time-up action (`limit_action`, default `logout`; `window_end_action`,
  default `shutdown`; chosen by `Status.cutoff`): agent asks Plasma
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
- **Login screen** (`login-theme.nix`, `sddm/ScreenTimeStatus.qml`): a patched
  copy of Breeze's SDDM theme, anchored on exact lines of its `Main.qml` with
  `substituteInPlace --replace-fail`, so a Plasma update that moves them fails
  the build, not the login screen. The banner gets data from `screentime
  status --json` via plasma5support's `executable` engine; it must hide on any
  error and never affect logging in. `loginScreen.enable` also covers the
  lock screen (`lock-screen.nix`): a full copy of plasma-desktop's
  `org.kde.plasma.desktop` shell package with `LockScreenUi.qml` patched,
  linked over the profile's copy in `environment.extraSetup`. Don't
  override single files with `lib.hiPrio`: KPackage rejects package files
  whose real path is outside the package directory ("Path traversal attempt
  detected") and kscreenlocker falls back to its built-in screen. Don't
  rename the shell either: Plasma keys desktop layouts on its name.
  `environment.extraSetup` also runs in every per-user profile
  (`users.users.<name>.packages`, which home-manager's `useUserPackages`
  fills), so anything in it must do nothing when its target is missing;
  test with a user that has `packages` set.
- Changes to Nix files or `sddm/*.qml` alone need no version bump or
  release: they're read from the repo at rebuild time, not shipped in the
  release tarball.
- Rendering the login theme without a desktop: build `kdePackages.sddm` with
  the Plasma 6 module's `extraPackages` (plus `plasma-workspace`,
  `plasma-desktop`, `breeze`; leave out `qtvirtualkeyboard`), point
  `login-theme.nix` at a stub `screentime` that `cat`s a JSON file, then run
  `sddm-greeter-qt6 --test-mode --theme DIR` under `Xvfb` with
  `QT_QUICK_BACKEND=software QT_XCB_GL_INTEGRATION=none QT_QPA_PLATFORM=xcb`,
  a fonts.conf with `noto-fonts`, a user with uid >= 1000, and
  `/etc/sddm.conf` containing `[General]` `InputMethod=` (else the virtual
  keyboard covers the screen). Screenshot with `xwd -root | convert`.
  Test mode can't attempt a login, so check the "Login Failed" path by
  calling `screentime.refusal(...)` from a temporary Timer in a copy.
  The lock screen renders the same way with
  `${kscreenlocker}/libexec/kscreenlocker_greet --testing --shell
  org.kde.plasma.desktop`, with `XDG_DATA_DIRS`, `QML_IMPORT_PATH` and
  `QT_PLUGIN_PATH` pointing at a `buildEnv` of the Plasma packages whose
  `postBuild` is the module's `extraSetup`. Without a window manager its
  window is 800x600 at the top left. "falling back to built-in locker" in
  its log means the QML didn't load.
- With the container's nix 2.26, `import "${pkgs.path}/..."` fails with
  "path ... is not valid"; import from the `fetchTarball` result instead.
- Real-world testing happens on the kid's PC: rebuild the config there, then
  `journalctl -u screentimed -f` and
  `sudo journalctl _SYSTEMD_USER_UNIT=screentime-agent.service`.
- The dev container used before had no nix or Rust by default and was reset
  often; reinstall as needed (`apt-get install nix-bin`, rustup). Unsandboxed
  nix builds there leave `/homeless-shelter` behind; delete it between builds.
