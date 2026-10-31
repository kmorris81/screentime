# SDDM theme "breeze-screentime": Plasma's Breeze login screen plus a banner
# with one user's screen time (sddm/ScreenTimeStatus.qml), and refused
# logins explained ("No screen time left today", "Outside allowed hours")
# instead of a bare "Login Failed".
#
# Breeze is copied from the system's plasma-desktop and patched at build
# time. If a Plasma update moves the lines patched below, the rebuild fails
# here (--replace-fail) rather than producing a broken login screen; then
# fix the anchors, or turn services.screentime.loginScreen off meanwhile.
{
  lib,
  runCommand,
  kdePackages,
  screentime,
  user,
}:

let
  command = "${screentime}/bin/screentime status --json ${lib.escapeShellArg user}";

  # Indented to sit among Main.qml's top-level items.
  indent =
    text:
    lib.concatMapStrings (l: if l == "" then "\n" else "        ${l}\n") (lib.splitString "\n" text);
in

runCommand "sddm-theme-breeze-screentime"
  {
    breeze = "${kdePackages.plasma-desktop}/share/sddm/themes/breeze";
    status = ./sddm/ScreenTimeStatus.qml;

    # Remember who tried to log in, to explain a failure. Both prompts (the
    # user list and "Other…") clear the message just before logging in.
    propertyAnchor = "    property string notificationMessage\n";
    propertyNew = "    property string lastLoginUser\n";
    loginAnchor = ''root.notificationMessage = ""'';
    loginNew = ''root.lastLoginUser = username; root.notificationMessage = ""'';

    failedAnchor = ''notificationMessage = i18ndc("plasma-desktop-sddm-theme", "@info:status", "Login Failed")'';
    failedNew = ''notificationMessage = screentime.refusal(root.lastLoginUser) || i18ndc("plasma-desktop-sddm-theme", "@info:status", "Login Failed")'';

    # The banner sits at the bottom centre, above the footer. Declared just
    # before the clock (top-level items of the login screen).
    clockAnchor = "        Clock {\n";
    banner = indent ''
      ScreenTimeStatus {
          id: screentime
          command: ${builtins.toJSON command}
          user: ${builtins.toJSON user}
          anchors.horizontalCenter: parent.horizontalCenter
          anchors.bottom: parent.bottom
          anchors.bottomMargin: Kirigami.Units.gridUnit * 4
      }
    '';
  }
  ''
    theme=$out/share/sddm/themes/breeze-screentime
    mkdir -p "$(dirname "$theme")"
    cp -r --no-preserve=mode "$breeze" "$theme"
    cp "$status" "$theme/ScreenTimeStatus.qml"

    cd "$theme"
    substituteInPlace Main.qml \
      --replace-fail "$propertyAnchor" "$propertyAnchor$propertyNew" \
      --replace-fail "$loginAnchor" "$loginNew" \
      --replace-fail "$failedAnchor" "$failedNew" \
      --replace-fail "$clockAnchor" "$banner$clockAnchor"
    sed -i -e 's/^Name=Breeze$/Name=Breeze (screen time)/' \
      -e 's/^Theme-Id=breeze$/Theme-Id=breeze-screentime/' metadata.desktop
  ''
