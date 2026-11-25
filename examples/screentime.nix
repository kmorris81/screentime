# Example screentime.nix for a kid's PC, imported next to the module:
#
#   imports = [ "${inputs.screentime}/module.nix" ./screentime.nix ];
#
# For a second PC, copy this file and change `kid`: everything below is
# keyed on it (e.g. "benny" on one machine, "eli" on the other).
#
# Server mode instead keeps the schedule on screentime-server (see
# README.md and policy.toml); the PC then only needs:
#
#   services.screentime = {
#     enable = true;
#     server.url = "http://pi.lan:8470";
#     users.benny = { };
#     loginScreen.enable = true;
#   };
let
  kid = "benny";
  weekday = {
    windows = [ "15:00-20:00" ];
    limitMinutes = 90;
  };
in
{
  services.screentime = {
    enable = true;
    idleMinutes = 5;

    limitAction = "logout"; # minutes used up
    windowEndAction = "shutdown"; # allowed window over

    # Banner on the SDDM login screen and the Plasma lock screen
    # ("Benny · 47m left"), and refused logins explained ("No screen time
    # left today", "Outside allowed hours"). Switches SDDM to the
    # "breeze-screentime" theme.
    loginScreen = {
      enable = true;
      user = kid; # the default when there's only one managed user
    };

    users.${kid}.days = {
      mon = weekday;
      tue = weekday;
      wed = weekday;
      thu = weekday;
      fri = {
        windows = [ "15:00-21:00" ];
        limitMinutes = 120;
      };
      sat = {
        windows = [
          "09:00-12:00"
          "14:00-21:00"
        ];
        limitMinutes = 180;
      };
      sun = {
        windows = [
          "09:00-12:00"
          "14:00-20:00"
        ];
        limitMinutes = 180;
      };
    };
  };
}
