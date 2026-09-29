{
  description = "Solana Protocol Adapter development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
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

          # Pinned to match CI exactly (dtolnay/rust-toolchain@<version> in ci.yml).
          rustToolchain = pkgs.rust-bin.stable."1.98.1".default;

          # nixos-26.05 ships anchor 1.0.2, not the 1.2.0 the programs build
          # with, so the CLI is built here with the pinned toolchain.
          anchorCli = (pkgs.makeRustPlatform {
            cargo = rustToolchain;
            rustc = rustToolchain;
          }).buildRustPackage (finalAttrs: {
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
          });

          solanaRelease = {
            "x86_64-linux" = {
              target = "x86_64-unknown-linux-gnu";
              releaseHash = "sha256-yXKJqKux0O+0l9i1yyhbqr2bf46mZH9dFFxdyP82Eeg=";
              platformToolsAsset = "linux-x86_64";
              platformToolsHash = "sha256-sPevEErfcm//KmoJ6i6y8tKWXJIpX01ziMCNFA4MKwA=";
            };
            "x86_64-darwin" = {
              target = "x86_64-apple-darwin";
              releaseHash = "sha256-RCTTmUBOxF1VUzxg0LUkt92a3cpxg8ZQt7EzgovPVYw=";
              platformToolsAsset = "osx-x86_64";
              platformToolsHash = "sha256-5vYjGxSeZK1swSYF0PmTQFzlWGKqREBsAjVjFZ+MPK8=";
            };
            "aarch64-darwin" = {
              target = "aarch64-apple-darwin";
              releaseHash = "sha256-C/vXaaVeMvCh/huS9282DwHuGUdfrPjDHLkPmdzKf+A=";
              platformToolsAsset = "osx-aarch64";
              platformToolsHash = "sha256-SMMsLsOsNym1yvH91sQUVJYSXt8EOyZisFhr+8kys0o=";
            };
          }.${system};

          solanaVersion = "4.3.0";

          # The platform-tools release cargo-build-sbf 4.3.0 and Anchor 1.2.0
          # both default to (Rust 1.95, LLVM 22).
          platformToolsVersion = "v1.57";

          platformTools = pkgs.stdenvNoCC.mkDerivation {
            pname = "platform-tools";
            version = platformToolsVersion;

            src = pkgs.fetchurl {
              url = "https://github.com/anza-xyz/platform-tools/releases/download/${platformToolsVersion}/platform-tools-${solanaRelease.platformToolsAsset}.tar.bz2";
              hash = solanaRelease.platformToolsHash;
            };

            nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [
              pkgs.autoPatchelfHook
            ];

            buildInputs = lib.optionals pkgs.stdenv.isLinux [
              pkgs.stdenv.cc.cc.lib
              pkgs.zlib
            ];

            dontUnpack = true;

            installPhase = ''
              runHook preInstall

              mkdir -p "$out"
              tar -xjf "$src" -C "$out"

              # The debugger (lldb, its library and Python bindings) is not
              # used to build, and liblldb links host libraries (Python 3.10,
              # ncurses, libedit, libxml2) that autoPatchelf cannot satisfy.
              rm -rf "$out/llvm/lib/liblldb"* "$out/llvm/lib/python"*
              rm -f "$out/llvm/bin/lldb"*

              runHook postInstall
            '';
          };

          solanaToolchain = pkgs.stdenvNoCC.mkDerivation {
            pname = "agave-release";
            version = solanaVersion;

            src = pkgs.fetchurl {
              url = "https://release.anza.xyz/v${solanaVersion}/solana-release-${solanaRelease.target}.tar.bz2";
              hash = solanaRelease.releaseHash;
            };

            nativeBuildInputs = lib.optionals pkgs.stdenv.isLinux [
              pkgs.autoPatchelfHook
            ];

            buildInputs = lib.optionals pkgs.stdenv.isLinux [
              pkgs.stdenv.cc.cc.lib
              pkgs.zlib
              pkgs.openssl
              pkgs.udev
            ];

            dontUnpack = true;

            installPhase = ''
              runHook preInstall

              mkdir -p "$out"
              tar -xjf "$src" --strip-components=1 -C "$out"

              # cargo-build-sbf only looks for platform-tools under
              # $HOME/.cache/solana/<version>/platform-tools, so the wrapper
              # links the Nix-built release there, then runs the real binary
              # against it: no download, and the platform-tools rustc/cargo
              # on PATH in place of a rustup toolchain.
              mv "$out/bin/cargo-build-sbf" "$out/bin/cargo-build-sbf-real"
              cat > "$out/bin/cargo-build-sbf" <<'EOF'
#!@bash@
set -euo pipefail

tools_link="$HOME/.cache/solana/@toolsVersion@/platform-tools"
if [[ -e "$tools_link" && ! -L "$tools_link" ]]; then
  echo "cargo-build-sbf: $tools_link is a directory, not the Nix platform-tools link." >&2
  echo "It was downloaded outside the Nix shell; remove it so the pinned release is used." >&2
  exit 1
fi
mkdir -p "$(dirname "$tools_link")"
ln -sfn "@platformTools@" "$tools_link"

if [[ "''${1:-}" == "build-sbf" ]]; then
  shift
fi

exec env PATH="@platformTools@/rust/bin:$PATH" "@out@/bin/cargo-build-sbf-real" \
  --no-rustup-override \
  --skip-tools-install \
  --disable-remap-cwd \
  "$@"
EOF
              substituteInPlace "$out/bin/cargo-build-sbf" \
                --subst-var out \
                --subst-var-by bash "${pkgs.bash}/bin/bash" \
                --subst-var-by platformTools "${platformTools}" \
                --subst-var-by toolsVersion "${platformToolsVersion}"
              chmod +x "$out/bin/cargo-build-sbf"

              runHook postInstall
            '';
          };
        in
        {
          packages.default = solanaToolchain;

          devShells.default = pkgs.mkShell {
            packages = [
              solanaToolchain
              rustToolchain
              anchorCli
              pkgs.nodejs_24
              pkgs.yarn
              pkgs.pkg-config
              pkgs.openssl
              pkgs.clang
              pkgs.llvm
              pkgs.cmake
              pkgs.protobuf
              pkgs.podman
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
            # The rootless container runtime podman drives on Linux; on macOS
            # podman runs containers inside its own `podman machine` VM.
            ++ lib.optionals pkgs.stdenv.isLinux [
              pkgs.udev
              pkgs.conmon
              pkgs.crun
              pkgs.fuse-overlayfs
              pkgs.slirp4netns
            ];

            shellHook = ''
              export CARGO_TERM_COLOR=always
              export RUST_BACKTRACE=1

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

              export CONTAINERS_REGISTRIES_CONF="$_nix_cache/containers/registries.conf"
              cat > "$CONTAINERS_REGISTRIES_CONF" <<'CONF'
              unqualified-search-registries = ["docker.io"]
              CONF

              ${lib.optionalString pkgs.stdenv.isLinux ''
                # Configure podman for rootless container execution (used by risc0 groth16 prover).
                # newuidmap is a setuid binary that nix cannot provide — it must come from the host.
                if ! command -v newuidmap >/dev/null; then
                  echo ""
                  echo "WARNING: newuidmap not found. Fixture generation (risc0 groth16 prover) will fail."
                  echo "Install it with: sudo apt install uidmap   (Debian/Ubuntu)"
                  echo "                 sudo dnf install shadow-utils  (Fedora/RHEL)"
                  echo ""
                fi

                export CONTAINERS_CONF="$_nix_cache/containers/containers.conf"
                export CONTAINERS_STORAGE_CONF="$_nix_cache/containers/storage.conf"

                cat > "$CONTAINERS_CONF" <<'CONF'
                [engine]
                runtime = "crun"
                CONF

                cat > "$CONTAINERS_STORAGE_CONF" <<CONF
                [storage]
                driver = "overlay"
                rootless_storage_path = "$_nix_cache/containers/storage"
                [storage.options.overlay]
                mount_program = "${pkgs.fuse-overlayfs}/bin/fuse-overlayfs"
                CONF
              ''}

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
