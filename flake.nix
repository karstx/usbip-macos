{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
    rust-overlay.url = "github:oxalica/rust-overlay";
    fenix.url = "github:nix-community/fenix/monthly";
  };

  outputs = { self, nixpkgs, utils, crane, rust-overlay, fenix }:
    utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];

        pkgs = import nixpkgs {
          inherit system overlays;
        };

        craneLib = (crane.mkLib pkgs).overrideToolchain (fenix.packages.${system}.fromToolchainFile {
          file = ./rust-toolchain.toml;
          sha256 = "sha256-+xUr6Al1wZSqX8aEsq7DpuFOK4Ve/i+qTl+K5uWQ9GM=";
        });

        commonArgs = {
          # The app embeds its menu bar icons, which cleanCargoSource alone would drop.
          src = pkgs.lib.cleanSourceWith {
            src = ./.;
            filter = path: type: pkgs.lib.hasSuffix ".svg" path || craneLib.filterCargoSources path type;
          };
          strictDeps = true;

          # .cargo/config.toml runs tests through sudo on macOS, which the build sandbox lacks.
          CARGO_TARGET_AARCH64_APPLE_DARWIN_RUNNER = "env";
          CARGO_TARGET_X86_64_APPLE_DARWIN_RUNNER = "env";

          buildInputs = pkgs.lib.optionals pkgs.stdenv.hostPlatform.isDarwin [
            pkgs.apple-sdk
          ];
        };

        usbip-macos = craneLib.buildPackage (
          commonArgs
          // {
            cargoArtifacts = craneLib.buildDepsOnly commonArgs;
          }
        );

        usbip-macos-app = pkgs.stdenvNoCC.mkDerivation {
          pname = "usbip-macos-app";
          inherit (usbip-macos) version;
          dontUnpack = true;
          installPhase = ''
            contents=$out/Applications/usbip.app/Contents
            mkdir -p $contents/MacOS $contents/Resources
            cp ${usbip-macos}/bin/usbip-macos $contents/MacOS/
            cp ${./packaging/AppIcon/Assets.car} $contents/Resources/Assets.car
            cp ${./packaging/AppIcon/AppIcon.icns} $contents/Resources/AppIcon.icns
            cp ${pkgs.replaceVars ./packaging/Info.plist { inherit (usbip-macos) version; }} $contents/Info.plist
          '';
        };

        package-app = pkgs.writeShellApplication {
          name = "package-app";
          runtimeEnv = {
            APP = "${usbip-macos-app}/Applications/usbip.app";
            ENTITLEMENTS = "${./entitlements.plist}";
            VERSION = usbip-macos.version;
          };
          text = builtins.readFile ./packaging/package-app.sh;
        };
      in
      {
        formatter = pkgs.nixpkgs-fmt;

        checks = {
          inherit usbip-macos;
        };

        packages = {
          default = usbip-macos;
        } // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          app = usbip-macos-app;
        };

        apps = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          package = {
            type = "app";
            program = "${package-app}/bin/package-app";
          };
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          RUST_BACKTRACE = 1;
        };
      }
    );
}
