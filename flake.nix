# This flake file is community maintained; adapted from swayward (see COPIED.md).
{
  description = "swaywardlock: A Wayland screen locker for the swayward-wm family.";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs =
    { self, nixpkgs }:
    let
      swaywardlock-package =
        {
          lib,
          rustPlatform,
          pkg-config,
          installShellFiles,
          scdoc,
          python3,
          cairo,
          pango,
          pam,
          libGL,
          libxkbcommon,
          wayland,
          fontconfig,
          makeFontsConf,
          liberation_ttf,
          mesa,
          coreutils,
        }:
        rustPlatform.buildRustPackage {
          pname = "swaywardlock";
          version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package.version;

          src = lib.fileset.toSource {
            root = ./.;
            fileset = lib.fileset.unions [
              ./src
              ./contrib
              ./docs
              ./examples
              ./pam
              ./Cargo.toml
              ./Cargo.lock
              ./LICENSE
              ./README.md
            ];
          };

          postPatch = ''
            patchShebangs contrib/swaylock-to-kdl contrib/hyprlock-to-kdl
          '';

          cargoLock = {
            allowBuiltinFetchGit = true;
            lockFile = ./Cargo.lock;
          };
          strictDeps = true;
          nativeBuildInputs = [
            pkg-config
            installShellFiles
            scdoc
          ];
          nativeCheckInputs = [
            python3
            coreutils
            fontconfig
          ];
          buildInputs = [
            cairo
            pango
            pam
            libGL
            libxkbcommon
            wayland
            python3
          ];

          # Keep both packages on real PAM. The debug package is not a test-auth build.
          buildNoDefaultFeatures = true;
          buildFeatures = [ ];

          preCheck = ''
            export XDG_RUNTIME_DIR="$(mktemp -d)"
            export FONTCONFIG_FILE=${makeFontsConf { fontDirectories = [ liberation_ttf ]; }}
            export LIBGL_DRIVERS_PATH=${mesa}/lib/dri
            export __EGL_VENDOR_LIBRARY_FILENAMES=${mesa}/share/glvnd/egl_vendor.d/50_mesa.json
            export LIBGL_ALWAYS_SOFTWARE=1
            # Nix libpam resolves bare stock module names relative to its store path.
            test -f ${pam}/lib/security/pam_succeed_if.so
            fc-match Sans
          '';
          # Show internal conditional skips, rather than calling them coverage.
          checkFlags = [ "--nocapture" ];

          postInstall = ''
            installShellCompletion --cmd swaywardlock \
              --bash <($out/bin/swaywardlock completions bash) \
              --fish <($out/bin/swaywardlock completions fish) \
              --zsh <($out/bin/swaywardlock completions zsh)
            scdoc < docs/swaywardlock.1.scd > swaywardlock.1
            installManPage swaywardlock.1
            install -Dm755 contrib/swaylock-to-kdl $out/bin/swaywardlock-swaylock-to-kdl
            install -Dm755 contrib/hyprlock-to-kdl $out/bin/swaywardlock-hyprlock-to-kdl
            mkdir -p $out/share/doc/swaywardlock
            cp -r docs examples pam $out/share/doc/swaywardlock/
            install -m644 README.md LICENSE $out/share/doc/swaywardlock/
            # No /etc/pam.d service is installed: NixOS generates its own stack.
            python3 contrib/check-doc-kdl $out/bin/swaywardlock
            for example in examples/*.kdl; do
              $out/bin/swaywardlock validate -c "$example"
            done
            for marker in swaywardlock-test SWAYWARDLOCK_TEST_SCREENCOPY \
              SWAYWARDLOCK_TEST_AUTH_DELAY_MS SWAYWARDLOCK_TEST_EXIT_PROBE \
              'test exit probe:' 'late test prompt' DROP_OBSERVER \
              'Drop must zero all 1024 bytes before munlock/deallocation'; do
              if grep -aFq "$marker" $out/bin/swaywardlock; then
                echo "FAIL: package contains test hook: $marker" >&2
                exit 1
              fi
            done
          '';

          env.RUSTFLAGS = toString (
            map (arg: "-C link-arg=" + arg) [
              # Required for dlopen() discovery through ELF NEEDED/RPATH.
              "-Wl,--push-state,--no-as-needed"
              "-lEGL"
              "-lwayland-client"
              "-lwayland-egl"
              "-Wl,--pop-state"
            ]
          );

          meta = {
            description = "Wayland screen locker for the swayward-wm family";
            homepage = "https://github.com/swayward-wm/swaywardlock";
            license = lib.licenses.gpl3Plus;
            mainProgram = "swaywardlock";
            platforms = lib.platforms.linux;
          };
        };

      inherit (nixpkgs) lib;
      systems = lib.intersectLists lib.systems.flakeExposed lib.platforms.linux;
      forAllSystems = lib.genAttrs systems;
      nixpkgsFor = forAllSystems (system: nixpkgs.legacyPackages.${system});
    in
    {
      checks = forAllSystems (system: {
        inherit (self.packages.${system}) swaywardlock-debug;
        swaywardlock-release = self.packages.${system}.swaywardlock;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = nixpkgsFor.${system};
          rustfmt' = pkgs.rustfmt.override { asNightly = true; };
          inherit (self.packages.${system}) swaywardlock;
        in
        {
          default = pkgs.mkShell {
            packages = [
              pkgs.rustc
              pkgs.cargo
              pkgs.clippy
              rustfmt'
              pkgs.python3
              pkgs.scdoc
            ];
            nativeBuildInputs = [ pkgs.pkg-config ];
            buildInputs = swaywardlock.buildInputs;
            # Append to this variable; replacing it breaks dlopen() library discovery.
            env.RUSTFLAGS = swaywardlock.RUSTFLAGS;
          };
        }
      );

      formatter = forAllSystems (system: nixpkgsFor.${system}.nixfmt-rfc-style);

      packages = forAllSystems (
        system:
        let
          swaywardlock = nixpkgsFor.${system}.callPackage swaywardlock-package { };
        in
        {
          inherit swaywardlock;
          swaywardlock-debug = swaywardlock.overrideAttrs (
            newAttrs: oldAttrs: {
              pname = oldAttrs.pname + "-debug";
              cargoBuildType = "debug";
              cargoCheckType = newAttrs.cargoBuildType;
              dontStrip = true;
            }
          );
          default = swaywardlock;
        }
      );

      overlays.default = final: _: {
        swaywardlock = final.callPackage swaywardlock-package { };
      };
    };
}
