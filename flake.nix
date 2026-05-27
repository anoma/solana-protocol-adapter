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

          # Rust 1.93.0: pinned to match CI exactly. Anchor 0.31.1 builds
          # fine on 1.85+ after its proc-macro2 fix.
          rustToolchain = pkgs.rust-bin.stable."1.93.0".default;

          # Nightly toolchain needed only for Anchor IDL generation
          # (anchor build calls `cargo +nightly` internally).
          rustNightly = pkgs.rust-bin.nightly.latest.minimal.override {
            extensions = [ "rust-src" ];
          };

          # ── risc0 guest-compilation toolchains ──────────────────────
          # risc0 publishes pre-built toolchains for x86_64-linux and
          # aarch64-darwin. x86_64-darwin cannot compile guests.
          risc0RustVersion = "r0.1.88.0";
          risc0CppVersion = "2024.01.05";

          risc0Platforms = {
            "x86_64-linux" = {
              target = "x86_64-unknown-linux-gnu";
              rustHash = "sha256-IiZReXuljwuv2VkZGkjSOzWzJm2eCC/En7TYRzMGT84=";
              cppAsset = "riscv32im-linux-x86_64";
              cppHash = "sha256-zBlJfbX9HM2S+j0xWjPKzUukgPjSGzyE37VJPP1o2g0=";
            };
            "aarch64-darwin" = {
              target = "aarch64-apple-darwin";
              rustHash = "sha256-kntua+pVWAgGxrk90D9TYKrMMuP+u+xcAOTTth+ynA0=";
              cppAsset = "riscv32im-osx-arm64";
              cppHash = "sha256-rx2x/H6otxROyBeLRGtQZv/6RaA5Jp4lEqDWuGACND8=";
            };
          };

          hasRisc0 = builtins.hasAttr system risc0Platforms;
          risc0Target = if hasRisc0 then risc0Platforms.${system}.target else "";

          risc0RustToolchain = if hasRisc0 then
            let info = risc0Platforms.${system}; in
            pkgs.stdenvNoCC.mkDerivation {
              pname = "risc0-rust-toolchain";
              version = risc0RustVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/risc0/rust/releases/download/${risc0RustVersion}/rust-toolchain-${info.target}.tar.gz";
                hash = info.rustHash;
              };
              nativeBuildInputs = [ pkgs.bash ]
                ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
              buildInputs = lib.optionals pkgs.stdenv.isLinux [
                pkgs.stdenv.cc.cc.lib
                pkgs.zlib
              ];
              dontUnpack = true;
              installPhase = ''
                runHook preInstall
                mkdir -p "$out"
                tar -xzf "$src" --strip-components=1 -C "$out"
                runHook postInstall
              '';
            }
          else null;

          risc0CppToolchain = if hasRisc0 then
            let info = risc0Platforms.${system}; in
            pkgs.stdenvNoCC.mkDerivation {
              pname = "risc0-cpp-toolchain";
              version = risc0CppVersion;
              src = pkgs.fetchurl {
                url = "https://github.com/risc0/toolchain/releases/download/${risc0CppVersion}/${info.cppAsset}.tar.xz";
                hash = info.cppHash;
              };
              nativeBuildInputs = [ pkgs.bash ]
                ++ lib.optionals pkgs.stdenv.isLinux [ pkgs.autoPatchelfHook ];
              buildInputs = lib.optionals pkgs.stdenv.isLinux [
                pkgs.stdenv.cc.cc.lib
                pkgs.zlib
                pkgs.libmpc
                pkgs.mpfr
                pkgs.gmp
              ];
              dontUnpack = true;
              installPhase = ''
                runHook preInstall
                mkdir -p "$out"
                tar -xJf "$src" --strip-components=1 -C "$out"
                runHook postInstall
              '';
            }
          else null;

          # Semver versions for settings.toml (rzup stores semver, not display strings)
          risc0RustSemver = "1.88.0";
          risc0CppSemver = "2024.1.5";

          risc0ShellHook = if hasRisc0 then ''
            # risc0 toolchain: create RISC0_HOME with Nix-provided toolchains.
            # rzup's find_version_dir filters out symlinked directories, so we
            # create real directories and symlink their contents.
            _risc0_home="$_nix_cache/risc0"
            mkdir -p "$_risc0_home/toolchains"

            _rust_dir="$_risc0_home/toolchains/rust_${risc0Target}_${risc0RustVersion}"
            rm -rf "$_rust_dir"
            mkdir -p "$_rust_dir"
            for item in "${risc0RustToolchain}"/*; do
              ln -sfn "$item" "$_rust_dir/$(basename "$item")"
            done

            _cpp_dir="$_risc0_home/toolchains/c_${risc0Target}_${risc0CppVersion}"
            rm -rf "$_cpp_dir"
            mkdir -p "$_cpp_dir"
            for item in "${risc0CppToolchain}"/*; do
              ln -sfn "$item" "$_cpp_dir/$(basename "$item")"
            done

            ln -sfn "$_cpp_dir" "$_risc0_home/cpp"
            touch "$_risc0_home/.rzup"
            {
              echo '[default_versions]'
              echo 'rust = "${risc0RustSemver}"'
              echo 'cpp = "${risc0CppSemver}"'
            } > "$_risc0_home/settings.toml"
            export RISC0_HOME="$_risc0_home"
          '' else ''
            # risc0 guest compilation not available on ${system}
            export RISC0_SKIP_BUILD=1
          '';

          # Cargo wrapper: dispatches to risc0's cargo for guest builds,
          # nightly for Anchor IDL, or default stable toolchain.
          cargoWrapper = pkgs.writeShellScriptBin "cargo" ''
            # risc0 guest build: RUSTC points to risc0 toolchain, use its cargo
            if [[ "''${RUSTC:-}" == *"risc0"* ]]; then
              _risc0_cargo="$(dirname "$RUSTC")/cargo"
              if [[ -x "$_risc0_cargo" ]]; then
                exec "$_risc0_cargo" "$@"
              fi
            fi
            # Anchor IDL: cargo +nightly
            if [ "''${1:-}" = "+nightly" ]; then
              shift
              exec env PATH="${rustNightly}/bin:$PATH" RUSTC="${rustNightly}/bin/rustc" "${rustNightly}/bin/cargo" "$@"
            fi
            # Default
            exec "${rustToolchain}/bin/cargo" "$@"
          '';

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

          devShells.default = pkgs.mkShell {
            packages = [
              solanaToolchain
              cargoWrapper
              rustToolchain
              pkgs.anchor
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

              ${risc0ShellHook}

              mkdir -p "$HOME/.config/solana"
              if [ ! -f "$HOME/.config/solana/id.json" ]; then
                echo "Generating Solana keypair at ~/.config/solana/id.json"
                solana-keygen new --no-bip39-passphrase -o "$HOME/.config/solana/id.json"
              fi

              solana config set --url localhost >/dev/null 2>&1 || true
            '';
          };
        });
}
