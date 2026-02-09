{
  description = "Solana Protocol Adapter development environment";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-25.05";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [
      "x86_64-linux"
      "x86_64-darwin"
      "aarch64-darwin"
    ]
      (system:
        let
          pkgs = import nixpkgs { inherit system; };
          lib = pkgs.lib;
          solanaRelease = {
            "x86_64-linux" = {
              target = "x86_64-unknown-linux-gnu";
              releaseHash = "sha256-Xf1CqPSixjGcTc9HkFgqooVzgl/hy4R1xw5Jf/oKq9Y=";
              platformToolsHash = "sha256-CTPgXdlkgm6OLbXFjDSuJV47rwzhcRVoVS3KgbVAems=";
              criterionVersion = "v2.3.3";
              criterionHash = "sha256-7n+S1yaFY4SKo66Qu/XfhmBLjQ+w5JQrDs+/YgFZZVA=";
            };
            "x86_64-darwin" = {
              target = "x86_64-apple-darwin";
              releaseHash = "sha256-tS392kyJjkkIATHKh+/azodnZN3mtAurnGaizS/nVS8=";
              platformToolsHash = "sha256-8efC4kD1eC8ONU/5CQDsTmNjogA7xGoPwZ2dMQhoyeA=";
              criterionVersion = "v2.3.2";
              criterionHash = "sha256-pw12DJO7CgNk1HaZxJgTFxF8oTX6vskURIvqAu3c7fI=";
            };
            "aarch64-darwin" = {
              target = "aarch64-apple-darwin";
              releaseHash = "sha256-T4gOyql5D5Q/NwCKa1HzDIlokgZI/s5hTIQ+gfePNjE=";
              platformToolsHash = "sha256-oeMrMTf+gZn6dtqBxD7AEivb0nrPEnnzjY0mX3pubLM=";
              criterionVersion = "v2.3.2";
              criterionHash = "sha256-pw12DJO7CgNk1HaZxJgTFxF8oTX6vskURIvqAu3c7fI=";
            };
          }.${system};

          platformToolsMachine = if pkgs.stdenv.isDarwin then "osx" else "linux";
          platformToolsArch = if system == "aarch64-darwin" then "aarch64" else "x86_64";
          criterionMachine = if pkgs.stdenv.isDarwin then "osx" else "linux";

          platformToolsSrc = pkgs.fetchurl {
            url = "https://github.com/anza-xyz/platform-tools/releases/download/v1.51/platform-tools-${platformToolsMachine}-${platformToolsArch}.tar.bz2";
            hash = solanaRelease.platformToolsHash;
          };

          criterionSrc = pkgs.fetchurl {
            url = "https://github.com/Snaipe/Criterion/releases/download/${solanaRelease.criterionVersion}/criterion-${solanaRelease.criterionVersion}-${criterionMachine}-x86_64.tar.bz2";
            hash = solanaRelease.criterionHash;
          };

          solanaToolchain = pkgs.stdenvNoCC.mkDerivation {
            pname = "agave-release";
            version = "3.0.13";

            src = pkgs.fetchurl {
              url = "https://release.anza.xyz/v3.0.13/solana-release-${solanaRelease.target}.tar.bz2";
              hash = solanaRelease.releaseHash;
            };

            nativeBuildInputs = [
              pkgs.bash
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
              touch "$out/bin/platform-tools-sdk/sbf/dependencies/platform-tools-v1.51.md"

              mkdir -p "$out/bin/platform-tools-sdk/sbf/dependencies/criterion"
              tar -xjf "${criterionSrc}" --strip-components=1 -C "$out/bin/platform-tools-sdk/sbf/dependencies/criterion"
              touch "$out/bin/platform-tools-sdk/sbf/dependencies/criterion-${solanaRelease.criterionVersion}.md"

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
          packages.solana-toolchain = solanaToolchain;

          devShells.default = pkgs.mkShell {
            packages = with pkgs; [
              solanaToolchain
              anchor
              cargo
              rustc
              rustfmt
              clippy
              cargo-risczero
              nodejs_20
              yarn
              pkg-config
              openssl
              clang
              llvm
              cmake
              protobuf
              git
              curl
              jq
              gnugrep
              gnused
              gawk
              findutils
              coreutils
              bashInteractive
            ]
            ++ lib.optionals stdenv.isLinux [
              udev
            ]
            ++ lib.optionals stdenv.isDarwin [
              libiconv
              darwin.apple_sdk.frameworks.Security
              darwin.apple_sdk.frameworks.SystemConfiguration
            ];

            shellHook = ''
              export CARGO_TERM_COLOR=always
              export RUST_BACKTRACE=1
              export SBF_SDK_PATH="${solanaToolchain}/bin/platform-tools-sdk/sbf"

              # In paths containing spaces, this injected rpath tokenization breaks linking.
              if [[ "''${NIX_LDFLAGS:-}" == *"/outputs/out/lib"* ]]; then
                unset NIX_LDFLAGS
              fi

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
