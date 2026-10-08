{ stdenv, lib, fetchurl, patchelf, niri, src ? fetchurl {
    url = "https://github.com/abdulrahman1s/appwarm/releases/download/v0.4.1/niri-appwarm-v0.4.1-x86_64-linux.tar.gz";
    hash = "sha256-npGx7CtWvRqMY0CZdHJRYUMlaIyi/San7Db62b057s8=";
  } }:

stdenv.mkDerivation {
  pname = "niri-appwarm-prebuilt";
  version = "26.04";
  inherit src;

  nativeBuildInputs = [ patchelf ];
  buildInputs = niri.buildInputs;
  dontUnpack = true;
  dontConfigure = true;
  dontBuild = true;

  installPhase = ''
    runHook preInstall
    mkdir -p "$out"
    tar -xzf "$src" -C "$out"
    chmod -R u+w "$out"
    patchelf --set-interpreter "${stdenv.cc.bintools.dynamicLinker}" \
      --set-rpath "${lib.makeLibraryPath ([ stdenv.cc.cc.lib ] ++ niri.buildInputs)}" \
      "$out/bin/niri"
    patchShebangs "$out/bin/niri-session"
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
