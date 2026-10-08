{ rustPlatform, lib, strace, makeWrapper }:

rustPlatform.buildRustPackage {
  pname = "appwarm";
  version = (builtins.fromTOML (builtins.readFile ../Cargo.toml)).package.version;
  src = lib.cleanSource ../.;
  cargoLock.lockFile = ../Cargo.lock;
  nativeBuildInputs = [ makeWrapper ];

  postInstall = ''
    wrapProgram "$out/bin/appwarm" --prefix PATH : "${lib.makeBinPath [ strace ]}"
  '';

  meta = {
    description = "Startup file profiler, page-cache warmer, and hidden execution staging";
    homepage = "https://github.com/abdulrahman1s/appwarm";
    license = lib.licenses.mit;
    mainProgram = "appwarm";
    platforms = lib.platforms.linux;
  };
}
