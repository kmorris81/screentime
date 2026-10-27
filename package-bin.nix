# The prebuilt static binaries from the GitHub release named in release.json
# (written by .github/workflows/release.yml), so nothing is compiled on the
# target machine. package.nix builds the same thing from source.
{
  lib,
  stdenvNoCC,
  fetchurl,
}:

let
  release = lib.importJSON ./release.json;
in

stdenvNoCC.mkDerivation {
  pname = "screentime";
  inherit (release) version;

  src = fetchurl { inherit (release) url hash; };

  dontConfigure = true;
  dontBuild = true;
  # Static and already stripped; nothing for patchelf to do.
  dontFixup = true;

  installPhase = ''
    mkdir -p $out
    cp -r bin share $out/
  '';

  meta = {
    description = "Screen-time limits for a KDE Plasma (Wayland) kid account";
    license = lib.licenses.mit;
    platforms = [ "x86_64-linux" ];
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
    mainProgram = "screentime";
  };
}
