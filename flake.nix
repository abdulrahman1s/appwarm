{
  description = "App startup profiling, cache warming, and hidden Niri staging";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  # The released Niri binary refers to store paths from this exact revision.
  # Keep it independent so consumers can make the main nixpkgs input follow theirs.
  inputs.nixpkgs-prebuilt.url = "github:NixOS/nixpkgs/151fa4e8ddfdd8dd25d945ad94ed54a13de9f6e4";

  outputs = { self, nixpkgs, nixpkgs-prebuilt }:
    let
      lib = nixpkgs.lib;
      systems = [ "x86_64-linux" "aarch64-linux" ];
      forSystems = lib.genAttrs systems;
      mkPkgs = system: import nixpkgs { inherit system; };
      mkPrebuiltPkgs = system: import nixpkgs-prebuilt { inherit system; };
    in
    {
      packages = forSystems (system:
        let pkgs = mkPkgs system;
        in rec {
          appwarm = pkgs.callPackage ./nix/package.nix { };
          niri-appwarm = pkgs.callPackage ./nix/niri-appwarm.nix { };
          default = appwarm;
        } // lib.optionalAttrs (system == "x86_64-linux") {
          niri-appwarm-prebuilt = (mkPrebuiltPkgs system).callPackage ./nix/prebuilt-niri.nix { };
        });

      apps = forSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.appwarm}/bin/appwarm";
          meta.description = "Run Appwarm";
        };
      });

      overlays.default = final: _prev: {
        appwarm = self.packages.${final.stdenv.hostPlatform.system}.appwarm;
      };

      lib.mkPrebuiltPackage = pkgs: { version ? "0.4.3", hash }:
        pkgs.stdenvNoCC.mkDerivation {
          pname = "appwarm-prebuilt";
          inherit version;
          src = pkgs.fetchurl {
            url = "https://github.com/abdulrahman1s/appwarm/releases/download/v${version}/appwarm-v${version}-x86_64-unknown-linux-musl.tar.gz";
            inherit hash;
          };
          sourceRoot = ".";
          dontConfigure = true;
          dontBuild = true;
          installPhase = ''
            install -Dm0755 appwarm "$out/bin/appwarm"
          '';
          meta = {
            description = "Statically linked Appwarm release binary";
            mainProgram = "appwarm";
            platforms = [ "x86_64-linux" ];
          };
        };

      nixosModules.default = import ./nix/module.nix { inherit self; };

      checks = forSystems (system: {
        package = self.packages.${system}.appwarm;
      });

      devShells = forSystems (system:
        let pkgs = mkPkgs system;
        in {
          default = pkgs.mkShell {
            packages = with pkgs; [ cargo rustc rustfmt clippy strace ];
          };
        });
    };
}
