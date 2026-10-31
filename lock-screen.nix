# The Plasma lock screen with the screen-time banner (sddm/ScreenTimeStatus.qml)
# for whoever is locked.
#
# The lock screen is LockScreenUi.qml in plasma-desktop's
# "org.kde.plasma.desktop" shell package. This builds a full copy of that
# package with the banner added, and `extraSetup` (for
# environment.extraSetup) points the system profile's
# share/plasma/shells/org.kde.plasma.desktop at it.
#
# Why not just override the one file with lib.hiPrio: the profile's package
# directory would then hold links into two store paths, and KPackage rejects
# files whose real path is outside the package ("Path traversal attempt"),
# so the lock screen wouldn't load. Why not a renamed shell: Plasma keys the
# desktop layout on the shell's name, so the user's panels would reset.
#
# If kscreenlocker can't load the QML it falls back to its built-in lock
# screen, so this can't lock anyone out. If a Plasma update moves the
# patched line, the rebuild fails here (--replace-fail).
#
# There's no "Unlocking failed" explanation: the lock screen only runs PAM's
# auth stack, not the account stack screentime's check lives in, so
# screentime never refuses an unlock.
{
  lib,
  runCommand,
  coreutils,
  kdePackages,
  screentime,
}:

let
  shell = "share/plasma/shells/org.kde.plasma.desktop";

  # Runs as the locked user, in their session.
  command = "${screentime}/bin/screentime status --json \"$(${coreutils}/bin/id -un)\"";

  # Indented to sit among LockScreenUi.qml's items.
  indent =
    text:
    lib.concatMapStrings (l: if l == "" then "\n" else "        ${l}\n") (lib.splitString "\n" text);

  pkg =
    runCommand "plasma-shell-desktop-screentime"
      {
        original = "${kdePackages.plasma-desktop}/${shell}";
        status = ./sddm/ScreenTimeStatus.qml;

        # Bottom centre, above the footer, like the SDDM banner. Declared
        # just before the clock.
        clockAnchor = "        Clock {\n";
        banner = indent ''
          ScreenTimeStatus {
              command: ${builtins.toJSON command}
              anchors.horizontalCenter: parent.horizontalCenter
              anchors.bottom: parent.bottom
              anchors.bottomMargin: Kirigami.Units.gridUnit * 4
          }
        '';

        # NixOS also runs extraSetup for each user's own package profile
        # (users.users.<name>.packages, home-manager's useUserPackages), so
        # it must do nothing where there's no Plasma shell package.
        passthru.extraSetup = profile: ''
          # screentime: the lock screen with the banner (lock-screen.nix).
          if [ -e ${profile}/${shell} ]; then
            # buildEnv may have linked a parent directory whole; make each
            # level a real directory so the package link can be replaced.
            dir=${profile}
            for part in share plasma shells; do
              dir=$dir/$part
              if [ -L "$dir" ]; then
                target=$(readlink -f "$dir")
                rm "$dir"
                mkdir "$dir"
                ln -s "$target"/* "$dir"/
              fi
            done
            rm -rf "$dir/org.kde.plasma.desktop"
            ln -s ${pkg}/${shell} "$dir/org.kde.plasma.desktop"
          fi
        '';
      }
      ''
        mkdir -p "$(dirname "$out/${shell}")"
        cp -r --no-preserve=mode "$original" "$out/${shell}"
        cp "$status" "$out/${shell}/contents/lockscreen/ScreenTimeStatus.qml"
        substituteInPlace "$out/${shell}/contents/lockscreen/LockScreenUi.qml" \
          --replace-fail "$clockAnchor" "$banner$clockAnchor"
      '';
in
pkg
