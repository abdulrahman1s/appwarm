{ stdenvNoCC, lib, fetchurl, pkgs, niri, archive ? fetchurl {
    url = "https://github.com/abdulrahman1s/appwarm/releases/download/v0.4.3/niri-appwarm-v0.4.3-x86_64-linux.tar.gz";
    hash = "sha256-WsCTRA/0r+pE/ztOy5ogdnLwCCiGphH0ae3nbJWZ+4s=";
  } }:

stdenvNoCC.mkDerivation {
  pname = "niri-appwarm-prebuilt";
  version = "26.04";
  src = archive;

  # These are the runtime outputs used by the pinned nixpkgs Niri build.
  # Its buildInputs select development outputs, which do not contain the .so files.
  buildInputs = with pkgs; map lib.getLib [
    libinput pango glib cairo pipewire libdisplay-info_0_3 libgbm
    seatd systemdMinimal pixman libxkbcommon libglvnd wayland glibc
    gcc.cc.lib bash
  ];
  dontUnpack = true;
  dontConfigure = true;
  dontBuild = true;
  dontMoveSystemdUserUnits = true;

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    tar -xzf "$src" -C "$out"
    chmod -R u+w "$out"
    sed -E -i "s@^ExecStart=/nix/store/[^/]+/bin/niri@ExecStart=$out/bin/niri@" \
      "$out/share/systemd/user/niri.service"
    grep -qF "ExecStart=$out/bin/niri --session" "$out/share/systemd/user/niri.service"
    runHook postInstall
  '';

  passthru = niri.passthru;
  meta = niri.meta // {
    description = "Prebuilt Niri with experimental Appwarm hidden-workspace support";
    platforms = [ "x86_64-linux" ];
  };
}
