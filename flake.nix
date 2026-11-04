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
          # The SDK's setup hook sets 14.0 and darwinMinVersionHook can only raise it, so this
          # lowers it after both have run.
          preConfigure = "export MACOSX_DEPLOYMENT_TARGET=10.15";
        };

        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        # The libc crate links libiconv, which the build takes from the store; Macs without Nix have it in /usr/lib.
        useSystemLibraries = exe: ''
          chmod u+w ${exe}
          for lib in $(otool -L ${exe} | awk '/^\t\/nix\/store\// { print $1 }'); do
            install_name_tool -change "$lib" "/usr/lib/$(basename "$lib")" ${exe}
          done
          if otool -L ${exe} | grep -q '^[[:space:]]/nix/store/'; then
            echo "${exe} still links libraries from the store" >&2
            exit 1
          fi
          codesign --force --sign - ${exe}
        '';

        usbip-macos = craneLib.buildPackage (commonArgs // { inherit cargoArtifacts; });

        # The Intel half of the app, cross-compiled: there is no x86_64-darwin toolchain to build it natively.
        intelArgs = commonArgs // {
          CARGO_BUILD_TARGET = "x86_64-apple-darwin";
          doCheck = false;
        };
        usbip-macos-intel = craneLib.buildPackage (
          intelArgs // { cargoArtifacts = craneLib.buildDepsOnly intelArgs; }
        );

        screenshot-server = craneLib.mkCargoDerivation (
          commonArgs
          // {
            pname = "usbip-screenshot-server";
            inherit (usbip-macos) version;
            inherit cargoArtifacts;
            buildPhaseCargoCommand = "cargo build --release --example screenshot_server";
            nativeBuildInputs = [ pkgs.cctools pkgs.darwin.sigtool ];
            installPhaseCommand = ''
              install -D target/release/examples/screenshot_server $out/bin/screenshot-server
              ${useSystemLibraries "$out/bin/screenshot-server"}
            '';
          }
        );

        usbip-macos-app = pkgs.stdenvNoCC.mkDerivation {
          pname = "usbip-macos-app";
          inherit (usbip-macos) version;
          dontUnpack = true;
          nativeBuildInputs = [ pkgs.cctools pkgs.darwin.sigtool ];
          installPhase = ''
            contents=$out/Applications/usbip.app/Contents
            mkdir -p $contents/MacOS $contents/Resources
            exe=$contents/MacOS/usbip-macos
            lipo -create ${usbip-macos}/bin/usbip-macos ${usbip-macos-intel}/bin/usbip-macos -output $exe
            ${useSystemLibraries "$exe"}
            # Info.plist says 10.15, where the controller API arrived; the linker raises the arm64
            # half to 11.0, the first release on Apple silicon.
            for expected in arm64:11.0 x86_64:10.15; do
              arch=''${expected%%:*}
              minos=$(otool -arch $arch -l $exe | awk '/LC_BUILD_VERSION/ { found = 1 } found && $1 == "minos" { print $2; exit }')
              if [ "$minos" != "''${expected#*:}" ]; then
                echo "the $arch half of $exe needs macOS $minos, expected ''${expected#*:}" >&2
                exit 1
              fi
            done
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

        screenshots = pkgs.writeShellApplication {
          name = "screenshots";
          runtimeEnv.SCREENSHOT_SERVER = "${screenshot-server}/bin/screenshot-server";
          text = builtins.readFile ./packaging/screenshots.sh;
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
          inherit screenshot-server;
        };

        apps = pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          package = {
            type = "app";
            program = "${package-app}/bin/package-app";
          };
          screenshots = {
            type = "app";
            program = "${screenshots}/bin/screenshots";
          };
        };

        devShells.default = craneLib.devShell {
          checks = self.checks.${system};

          RUST_BACKTRACE = 1;
        };
      }
    );
}
