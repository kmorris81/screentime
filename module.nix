# NixOS module for screentime: the system daemon, the per-session agent
# (idle tracking + tray countdown), and the PAM check that refuses logins
# when there's no time left.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.services.screentime;
  inherit (lib) mkOption types;

  dayNames = [
    "mon"
    "tue"
    "wed"
    "thu"
    "fri"
    "sat"
    "sun"
  ];

  actionType = types.enum [
    "shutdown"
    "logout"
    "lock"
  ];

  dayType = types.submodule {
    options = {
      windows = mkOption {
        type = types.listOf types.str;
        default = [ ];
        example = [
          "09:00-12:00"
          "14:00-20:00"
        ];
        description = "Allowed time windows, HH:MM-HH:MM (end exclusive, 24:00 allowed).";
      };
      limitMinutes = mkOption {
        type = types.ints.unsigned;
        default = 0;
        description = "Screen time allowed on this day, in minutes.";
      };
    };
  };

  # Use the GitHub release binaries only if release.json describes this
  # very version; between a version bump and its release, build from source.
  prebuiltAvailable =
    builtins.pathExists ./release.json
    && (lib.importJSON ./release.json).version == (lib.importTOML ./Cargo.toml).package.version
    && pkgs.stdenv.hostPlatform.system == "x86_64-linux";

  loginTheme = pkgs.callPackage ./login-theme.nix {
    screentime = cfg.package;
    inherit (cfg.loginScreen) user;
  };

  settings = {
    idle_timeout_secs = cfg.idleMinutes * 60;
    input_idle_only = cfg.inputIdleOnly;
    logout_grace_secs = cfg.logoutGraceSeconds;
    force_logout_secs = cfg.forceLogoutSeconds;
    limit_action = cfg.limitAction;
    window_end_action = cfg.windowEndAction;
    agent_path = "${cfg.package}/bin/screentime-agent";
    users = lib.mapAttrs (_: user: {
      days = lib.mapAttrs (_: day: {
        inherit (day) windows;
        limit_minutes = day.limitMinutes;
      }) user.days;
    }) cfg.users;
  };

  # Validated at build time: bad windows fail the rebuild, not the daemon.
  configFile =
    pkgs.runCommand "screentime.toml"
      {
        raw = (pkgs.formats.toml { }).generate "screentime-raw.toml" settings;
      }
      ''
        ${cfg.package}/bin/screentimed --check "$raw"
        cp "$raw" "$out"
      '';
in

{
  imports = [
    (lib.mkRemovedOptionModule [ "services" "screentime" "timeUpAction" ]
      "Use services.screentime.limitAction (default \"logout\") and windowEndAction (default \"shutdown\")."
    )
  ];

  options.services.screentime = {
    enable = lib.mkEnableOption "screen-time limits";

    package = mkOption {
      type = types.package;
      default =
        if prebuiltAvailable then
          pkgs.callPackage ./package-bin.nix { }
        else
          pkgs.callPackage ./package.nix { };
      defaultText = lib.literalMD "the prebuilt release binaries when they match this version, else built from source";
      description = ''
        The screentime package. Set it to
        `pkgs.callPackage "''${inputs.screentime}/package.nix" { }` to always
        build from source.
      '';
    };

    idleMinutes = mkOption {
      type = types.ints.positive;
      default = 5;
      description = "Minutes without keyboard/mouse input after which the counter stops.";
    };

    inputIdleOnly = mkOption {
      type = types.bool;
      default = true;
      description = ''
        Only real input counts as activity. When false, apps that inhibit
        idle (video players, games) keep the counter running without input.
      '';
    };

    limitAction = mkOption {
      type = actionType;
      default = "logout";
      description = ''
        What happens when a user's minutes for the day run out (or an admin
        runs `screentime end`): power the machine off, log the user out, or
        lock the screen (locked again whenever they unlock it). Refused
        logins afterwards never trigger this.
      '';
    };

    windowEndAction = mkOption {
      type = actionType;
      default = "shutdown";
      description = ''
        What happens when a user's allowed window ends while they're logged
        in; same choices as `limitAction`. If the window ends while an
        earlier `limitAction` is in effect (e.g. a locked screen), this
        action follows, with its own grace period.
      '';
    };

    logoutGraceSeconds = mkOption {
      type = types.ints.unsigned;
      default = 30;
      description = "Seconds between running out of time and the clean Plasma logout, shutdown or lock.";
    };

    forceLogoutSeconds = mkOption {
      type = types.ints.positive;
      default = 20;
      description = ''
        If that hasn't happened this long after the grace period (agent
        killed, logout cancelled by an app), the daemon does it through
        logind: terminates the sessions, powers off, or locks the sessions.
      '';
    };

    users = mkOption {
      type = types.attrsOf (
        types.submodule {
          options.days = mkOption {
            type = types.attrsOf dayType;
            default = { };
            description = "Per-day rules keyed mon..sun. Missing days aren't allowed.";
          };
        }
      );
      default = { };
      description = "Users whose screen time is limited.";
    };

    pamServices = mkOption {
      type = types.listOf types.str;
      # On NixOS sddm's account stack is `include login`, so this covers
      # graphical and TTY logins alike.
      default = [ "login" ];
      description = "PAM services that refuse logins for users without time left.";
    };

    loginScreen = {
      enable = lib.mkEnableOption ''
        a screen-time banner on the SDDM login screen, and an explanation
        when screentime refuses a login. This switches SDDM to a patched
        copy of Plasma's Breeze theme ("breeze-screentime")'';

      user = mkOption {
        type = types.nullOr types.str;
        default =
          let
            names = lib.attrNames cfg.users;
          in
          if lib.length names == 1 then lib.head names else null;
        defaultText = lib.literalMD "the managed user, if there is exactly one";
        example = "benny";
        description = "Managed user whose screen time the login screen shows.";
      };
    };
  };

  config = lib.mkIf cfg.enable (
    lib.mkMerge [
      {
        assertions = lib.mapAttrsToList (name: user: {
          assertion = lib.all (d: lib.elem d dayNames) (lib.attrNames user.days);
          message = "services.screentime.users.${name}.days: day names must be one of ${lib.concatStringsSep ", " dayNames}";
        }) cfg.users;

        environment.etc."screentime/config.toml".source = configFile;
        environment.systemPackages = [ cfg.package ];
        services.dbus.packages = [ cfg.package ];

        systemd.services.screentimed = {
          description = "Screen-time limits";
          wantedBy = [ "multi-user.target" ];
          after = [
            "dbus.service"
            "systemd-logind.service"
          ];
          restartTriggers = [ configFile ];
          serviceConfig = {
            Type = "dbus";
            BusName = "org.screentime.Daemon1";
            ExecStart = "${cfg.package}/bin/screentimed";
            Restart = "always";
            RestartSec = 2;
            StateDirectory = "screentime";
          };
        };

        # Runs in each managed user's desktop session. Stopping it doesn't help:
        # without heartbeats the daemon counts the user as active.
        systemd.user.services.screentime-agent = {
          description = "Screen-time tray countdown and idle tracking";
          wantedBy = [ "graphical-session.target" ];
          partOf = [ "graphical-session.target" ];
          after = [ "graphical-session.target" ];
          unitConfig.ConditionUser = map (u: "|${u}") (lib.attrNames cfg.users);
          serviceConfig = {
            ExecStart = "${cfg.package}/bin/screentime-agent";
            Restart = "always";
            RestartSec = 5;
          };
        };

        security.pam.services = lib.genAttrs cfg.pamServices (service: {
          rules.account.screentime = {
            control = "required";
            modulePath = "${pkgs.pam}/lib/security/pam_exec.so";
            args = [
              "quiet"
              "stdout"
              "${cfg.package}/bin/screentime"
              "pam-check"
            ];
            # First in the stack, ahead of any `sufficient` rules that could skip it
            order = 10000;
          };
        });
      }

      (lib.mkIf cfg.loginScreen.enable {
        assertions = [
          {
            assertion = cfg.loginScreen.user != null && cfg.users ? ${toString cfg.loginScreen.user};
            message = "services.screentime.loginScreen.user must name one of services.screentime.users (${lib.concatStringsSep ", " (lib.attrNames cfg.users)}).";
          }
        ];

        environment.systemPackages = [ loginTheme ];
        services.displayManager.sddm = {
          theme = "breeze-screentime";
          # The banner's data engine; Plasma's module adds it too.
          extraPackages = [ pkgs.kdePackages.plasma5support ];
          # What the sddm module sets for theme = "breeze".
          settings.Theme = {
            CursorTheme = lib.mkDefault "breeze_cursors";
            CursorSize = lib.mkDefault 24;
          };
        };
      })
    ]
  );
}
