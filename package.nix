{
  lib,
  rustPlatform,
}:

rustPlatform.buildRustPackage {
  pname = "screentime";
  version = "0.1.0";

  # Only the files cargo needs, so a local target/ never ends up in the build
  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./src
      ./examples
      ./dbus
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  postInstall = ''
    install -Dm644 dbus/org.screentime.Daemon1.conf \
      $out/share/dbus-1/system.d/org.screentime.Daemon1.conf
  '';

  meta = {
    description = "Screen-time limits for a KDE Plasma (Wayland) kid account";
    license = lib.licenses.mit;
    platforms = lib.platforms.linux;
    mainProgram = "screentime";
  };
}
