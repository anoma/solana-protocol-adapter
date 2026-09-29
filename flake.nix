{
  description = "Solana Protocol Adapter development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ]
      (system:
        let
          pkgs = import nixpkgs {
            inherit system;
            overlays = [ rust-overlay.overlays.default ];
          };
          lib = pkgs.lib;

          # Rust 1.93.0: pinned to match CI exactly; Anchor 1.2.0 needs 1.89+.
          rustToolchain = pkgs.rust-bin.stable."1.93.0".default;

          rustPlatform = pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          };

          # nixos-25.05 ships anchor 0.31.1, and its rustc is below Anchor
          # 1.2.0's MSRV, so the CLI is built here with the pinned toolchain.
          anchorCli = rustPlatform.buildRustPackage (finalAttrs: {
            pname = "anchor";
            version = "1.2.0";

            src = pkgs.fetchFromGitHub {
              owner = "otter-sec";
              repo = "anchor";
              tag = "v${finalAttrs.version}";
              hash = "sha256-lbNAMEqRYkyRojs8r9pDZI36DTBzHuyP7LSvHd5cZi8=";
              fetchSubmodules = true;
            };

            cargoHash = "sha256-8AX5G2j9KMjq6vaby4/RGXXSDHNwJsYiEYHJsoeDJaM=";

            cargoBuildFlags = [ "-p" "anchor-cli" ];
            cargoTestFlags = [ "-p" "anchor-cli" ];

            meta = {
              description = "Solana Sealevel Framework";
              homepage = "https://github.com/otter-sec/anchor";
              license = lib.licenses.asl20;
              mainProgram = "anchor";
            };
          });

          solanaRelease = {
            "x86_64-linux" = {
              target = "x86_64-unknown-linux-gnu";
              releaseHash = "sha256-Bvl8BlzJd8vsLxP/ybydO5L+9IVDH8s3Ciad5pUy71E=";
              platformToolsHash = "sha256-izhh6T2vCF7BK2XE+sN02b7EWHo94Whx2msIqwwdkH4=";
              criterionVersion = "v2.3.3";
              criterionHash = "sha256-7n+S1yaFY4SKo66Qu/XfhmBLjQ+w5JQrDs+/YgFZZVA=";
            };
            "x86_64-darwin" = {
              target = "x86_64-apple-darwin";
              releaseHash = "sha256-43aO0B2qHjz8Aq8+PrOWzsLUipns+AzV173/UQ+AjR8=";
              platformToolsHash = "sha256-HdTysfe1MWwvGJjzfHXtSV7aoIMzM0kVP+lV5Wg3kdE=";
              criterionVersion = "v2.3.2";
              criterionHash = "sha256-pw12DJO7CgNk1HaZxJgTFxF8oTX6vskURIvqAu3c7fI=";
            };
            "aarch64-darwin" = {
              target = "aarch64-apple-darwin";
              releaseHash = "sha256-VM/CaAvWQm/aBGGe4Bkz9ApknIBW86Ybog3FTdQn6+0=";
              platformToolsHash = "sha256-Fyffsx6DPOd30B5wy0s869JrN2vwnYBSfwJFfUz2/QA=";
              criterionVersion = "v2.3.2";
              criterionHash = "sha256-pw12DJO7CgNk1HaZxJgTFxF8oTX6vskURIvqAu3c7fI=";
            };
          }.${system};

          platformToolsMachine = if pkgs.stdenv.isDarwin then "osx" else "linux";
          platformToolsArch = if system == "aarch64-darwin" then "aarch64" else "x86_64";

          platformToolsSrc = pkgs.fetchurl {
            url = "https://github.com/anza-xyz/platform-tools/releases/download/v1.52/platform-tools-${platformToolsMachine}-${platformToolsArch}.tar.bz2";
            hash = solanaRelease.platformToolsHash;
          };

          criterionSrc = pkgs.fetchurl {
            url = "https://github.com/Snaipe/Criterion/releases/download/${solanaRelease.criterionVersion}/criterion-${solanaRelease.criterionVersion}-${platformToolsMachine}-x86_64.tar.bz2";
            hash = solanaRelease.criterionHash;
          };

          solanaVersion = "3.1.14";

          solanaToolchain = pkgs.stdenvNoCC.mkDerivation {
            pname = "agave-release";
            version = solanaVersion;

            src = pkgs.fetchurl {
              url = "https://release.anza.xyz/v${solanaVersion}/solana-release-${solanaRelease.target}.tar.bz2";
              hash = solanaRelease.releaseHash;
            };

            nativeBuildInputs = [
              pkgs.bash
            ] ++ lib.optionals pkgs.stdenv.isLinux [
              pkgs.autoPatchelfHook
            ];

            buildInputs = lib.optionals pkgs.stdenv.isLinux [
              pkgs.stdenv.cc.cc.lib
              pkgs.zlib
              pkgs.libffi
              pkgs.openssl
              pkgs.udev
            ];

            dontUnpack = true;

            installPhase = ''
              runHook preInstall

              mkdir -p "$out"
              tar -xjf "$src" --strip-components=1 -C "$out"

              mkdir -p "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools"
              tar -xjf "${platformToolsSrc}" --strip-components=1 -C "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools"
              mkdir -p "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/bin"
              cat > "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/bin/lldb-argdumper" <<'EOF'
#!${pkgs.bash}/bin/bash
set -euo pipefail
exec "$(dirname "$0")/../llvm/bin/lldb" "$@"
EOF
              chmod +x "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/bin/lldb-argdumper"
              ln -sfn llvm/lib "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/lib"
              touch "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools-v1.52.md"

              mkdir -p "$out/bin/platform-tools-sdk/sbf/dependencies/criterion"
              tar -xjf "${criterionSrc}" --strip-components=1 -C "$out/bin/platform-tools-sdk/sbf/dependencies/criterion"
              touch "$out/bin/platform-tools-sdk/sbf/dependencies/criterion-${solanaRelease.criterionVersion}.md"

              # Remove optional components with unsatisfiable deps (SGX, CUDA, lldb)
              rm -rf "$out/bin/perf-libs"
              rm -rf "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/llvm/lib/liblldb"*
              rm -rf "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/llvm/lib/python3.10"
              rm -f "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools/llvm/bin/lldb"*

              mv "$out/bin/cargo-build-sbf" "$out/bin/cargo-build-sbf-real"
              cat > "$out/bin/cargo-build-sbf" <<'EOF'
#!@bash@
set -euo pipefail

REAL_BIN="@out@/bin/cargo-build-sbf-real"
DEFAULT_SDK="@out@/bin/platform-tools-sdk/sbf"
SBF_SDK="''${SBF_SDK_PATH:-$DEFAULT_SDK}"
TOOLCHAIN_BIN="$SBF_SDK/dependencies/platform-tools/rust/bin"

if [[ ! -x "$TOOLCHAIN_BIN/rustc" ]]; then
  echo "Missing Solana SBF rust toolchain at: $TOOLCHAIN_BIN" >&2
  exit 1
fi

if [[ "''${1:-}" == "build-sbf" ]]; then
  shift
  exec env PATH="$TOOLCHAIN_BIN:$PATH" "$REAL_BIN" \
    build-sbf \
    --no-rustup-override \
    --skip-tools-install \
    --disable-remap-cwd \
    --sbf-sdk "$SBF_SDK" \
    "$@"
fi

exec env PATH="$TOOLCHAIN_BIN:$PATH" "$REAL_BIN" \
  --no-rustup-override \
  --skip-tools-install \
  --disable-remap-cwd \
  --sbf-sdk "$SBF_SDK" \
  "$@"
EOF
              substituteInPlace "$out/bin/cargo-build-sbf" \
                --subst-var out \
                --subst-var-by bash "${pkgs.bash}/bin/bash"
              chmod +x "$out/bin/cargo-build-sbf"

              runHook postInstall
            '';
          };
        in
        {
          packages.default = solanaToolchain;

          # scripts/verifier-idls.sh builds the RISC Zero verifier IDLs from
          # risc0-solana v3.0.0, an Anchor 0.31 workspace. Anchor 0.31's IDL
          # build resolves that source's type aliases only under
          # `cargo +nightly`, so it runs with nixos-25.05's anchor 0.31.1 and a
          # nightly toolchain, apart from the default shell's Anchor 1.2.0.
          devShells.verifier-idls =
            let
              rustNightly = pkgs.rust-bin.nightly.latest.minimal;
              # `cargo +nightly` dispatches to the Nix-provided nightly
              # toolchain instead of relying on rustup.
              cargoWrapper = pkgs.writeShellScriptBin "cargo" ''
                if [ "''${1:-}" = "+nightly" ]; then
                  shift
                  exec env PATH="${rustNightly}/bin:$PATH" RUSTC="${rustNightly}/bin/rustc" "${rustNightly}/bin/cargo" "$@"
                fi
                exec "${rustToolchain}/bin/cargo" "$@"
              '';
            in
            pkgs.mkShell {
              packages = [
                cargoWrapper
                rustToolchain
                pkgs.anchor
                pkgs.git
              ];
            };

          devShells.default = pkgs.mkShell {
            packages = [
              solanaToolchain
              rustToolchain
              anchorCli
              pkgs.cargo-risczero
              pkgs.nodejs_20
              pkgs.yarn
              pkgs.pkg-config
              pkgs.openssl
              pkgs.clang
              pkgs.llvm
              pkgs.cmake
              pkgs.protobuf
              pkgs.podman
              pkgs.conmon
              pkgs.crun
              pkgs.fuse-overlayfs
              pkgs.slirp4netns
              pkgs.git
              pkgs.curl
              pkgs.jq
              pkgs.gnugrep
              pkgs.gnused
              pkgs.gawk
              pkgs.findutils
              pkgs.coreutils
              pkgs.bashInteractive
            ]
            ++ lib.optionals pkgs.stdenv.isLinux [
              pkgs.udev
            ]
            ++ lib.optionals pkgs.stdenv.isDarwin [
              pkgs.libiconv
              pkgs.darwin.apple_sdk.frameworks.Security
              pkgs.darwin.apple_sdk.frameworks.SystemConfiguration
            ];

            shellHook = ''
              export CARGO_TERM_COLOR=always
              export RUST_BACKTRACE=1
              export SBF_SDK_PATH="${solanaToolchain}/bin/platform-tools-sdk/sbf"

              # In paths containing spaces, this injected rpath tokenization breaks linking.
              if [[ "''${NIX_LDFLAGS:-}" == *"/outputs/out/lib"* ]]; then
                unset NIX_LDFLAGS
              fi

              # Provide "docker" command via podman so risc0's groth16 prover works
              # without a host Docker installation. Podman runs rootless and daemonless.
              _nix_cache="$HOME/.cache/solana-pa-nix"
              mkdir -p "$_nix_cache/bin" "$_nix_cache/containers"

              cat > "$_nix_cache/bin/docker" <<'WRAPPER'
              #!/usr/bin/env bash
              exec podman "$@"
              WRAPPER
              chmod +x "$_nix_cache/bin/docker"
              export PATH="$_nix_cache:$_nix_cache/bin:$PATH"

              # Configure podman for rootless container execution (used by risc0 groth16 prover).
              # newuidmap is a setuid binary that nix cannot provide — it must come from the host.
              if ! command -v newuidmap >/dev/null 2>&1; then
                echo ""
                echo "WARNING: newuidmap not found. Fixture generation (risc0 groth16 prover) will fail."
                echo "Install it with: sudo apt install uidmap   (Debian/Ubuntu)"
                echo "                 sudo dnf install shadow-utils  (Fedora/RHEL)"
                echo ""
              fi

              export CONTAINERS_CONF="$_nix_cache/containers/containers.conf"
              export CONTAINERS_STORAGE_CONF="$_nix_cache/containers/storage.conf"
              export CONTAINERS_REGISTRIES_CONF="$_nix_cache/containers/registries.conf"

              cat > "$CONTAINERS_CONF" <<'CONF'
              [engine]
              runtime = "crun"
              CONF

              cat > "$CONTAINERS_STORAGE_CONF" <<CONF
              [storage]
              driver = "overlay"
              rootless_storage_path = "$_nix_cache/containers/storage"
              [storage.options.overlay]
              mount_program = "$(command -v fuse-overlayfs)"
              CONF

              cat > "$CONTAINERS_REGISTRIES_CONF" <<'CONF'
              unqualified-search-registries = ["docker.io"]
              CONF

              mkdir -p "$HOME/.config/containers"
              if [ ! -f "$HOME/.config/containers/policy.json" ]; then
                cat > "$HOME/.config/containers/policy.json" <<'POLICY'
              {"default": [{"type": "insecureAcceptAnything"}]}
              POLICY
              fi

              mkdir -p "$HOME/.config/solana"
              if [ ! -f "$HOME/.config/solana/id.json" ]; then
                echo "Generating Solana keypair at ~/.config/solana/id.json"
                solana-keygen new --no-bip39-passphrase -o "$HOME/.config/solana/id.json"
              fi

              if ! solana config set --url localhost >/dev/null; then
                echo "ERROR: 'solana config set --url localhost' failed; see the message above." >&2
                exit 1
              fi
            '';
          };
        });
}
