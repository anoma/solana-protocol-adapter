use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use risc0_binfmt::{compute_image_id, ProgramBinary};
use risc0_zkos_v1compat::V1COMPAT_ELF;

const TARGET_TRIPLE: &str = "riscv32im-risc0-zkvm-elf";
const HOST_PKG: &str = "passthrough-logic-methods";
const GUEST_PKG: &str = "passthrough-logic-guest";
const DEFAULT_DOCKER_TAG: &str = "r0.1.88.0";
const USER_TEXT_START: u32 = 0x0020_0800;

fn main() {
    println!("cargo:rerun-if-changed=guest/Cargo.toml");
    println!("cargo:rerun-if-changed=guest/Cargo.lock");
    println!("cargo:rerun-if-changed=guest/src/main.rs");
    println!("cargo:rerun-if-env-changed=RISC0_DOCKER_CONTAINER_TAG");

    let manifest_dir = PathBuf::from(
        env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is not set for build script"),
    );
    let guest_target_dir = get_out_dir().join(HOST_PKG).join(GUEST_PKG);

    build_guest_in_docker(&manifest_dir, &guest_target_dir);

    let user_elf_path = guest_target_dir
        .join(TARGET_TRIPLE)
        .join("release")
        .join(GUEST_PKG);
    let user_elf = fs::read(&user_elf_path).unwrap_or_else(|err| {
        panic!(
            "failed reading guest ELF at {}: {err}",
            user_elf_path.display()
        )
    });

    let combined_binary = ProgramBinary::new(&user_elf, V1COMPAT_ELF).encode();
    let combined_path = user_elf_path.with_extension("bin");
    fs::write(&combined_path, &combined_binary).unwrap_or_else(|err| {
        panic!(
            "failed writing combined guest binary at {}: {err}",
            combined_path.display()
        )
    });

    let image_id =
        compute_image_id(&combined_binary).expect("failed computing image id for passthrough guest");
    let image_id_words: [u32; 8] = image_id
        .as_words()
        .try_into()
        .expect("unexpected image id word count");

    let methods_rs = format!(
        "pub const PASSTHROUGH_LOGIC_GUEST_ELF: &[u8] = include_bytes!({:?});\n\
         pub const PASSTHROUGH_LOGIC_GUEST_ID: [u32; 8] = {:?};\n",
        combined_path, image_id_words
    );

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR is not set for build script"));
    fs::write(out_dir.join("methods.rs"), methods_rs)
        .expect("failed writing generated methods.rs for passthrough guest");
}

fn build_guest_in_docker(manifest_dir: &Path, guest_target_dir: &Path) {
    let use_podman = detect_container_runtime();
    fs::create_dir_all(guest_target_dir).unwrap_or_else(|err| {
        panic!(
            "failed creating guest target dir {}: {err}",
            guest_target_dir.display()
        )
    });

    let docker_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| panic!("failed to compute fixture-gen root from {}", manifest_dir.display()));
    let guest_manifest = manifest_dir.join("guest").join("Cargo.toml");
    let guest_manifest_rel = guest_manifest
        .strip_prefix(docker_root)
        .unwrap_or_else(|_| {
            panic!(
                "guest manifest {} is not under docker root {}",
                guest_manifest.display(),
                docker_root.display()
            )
        })
        .to_string_lossy()
        .to_string();
    let target_dir_rel = guest_target_dir
        .strip_prefix(docker_root)
        .unwrap_or_else(|_| {
            panic!(
                "guest target dir {} is not under docker root {}",
                guest_target_dir.display(),
                docker_root.display()
            )
        })
        .to_string_lossy()
        .to_string();

    run_docker_cargo(
        docker_root,
        &guest_manifest_rel,
        &target_dir_rel,
        "fetch",
        &[],
    );
    run_docker_cargo(
        docker_root,
        &guest_manifest_rel,
        &target_dir_rel,
        "build",
        &["--release"],
    );
    normalize_output_ownership(docker_root, &target_dir_rel, use_podman);
}

fn run_docker_cargo(
    docker_root: &Path,
    guest_manifest_rel: &str,
    target_dir_rel: &str,
    subcommand: &str,
    extra: &[&str],
) {
    let image = format!("risczero/risc0-guest-builder:{}", docker_tag());
    let volume = format!("{}:/src", docker_root.display());
    let mut cargo_args = vec![
        "cargo".to_string(),
        "+risc0".to_string(),
        subcommand.to_string(),
        "--locked".to_string(),
        "--target".to_string(),
        TARGET_TRIPLE.to_string(),
        "--manifest-path".to_string(),
        guest_manifest_rel.to_string(),
    ];
    if subcommand != "fetch" {
        cargo_args.push("--target-dir".to_string());
        cargo_args.push(target_dir_rel.to_string());
    }
    cargo_args.extend(extra.iter().map(|arg| (*arg).to_string()));
    let docker_command = cargo_args.join(" ");
    let encoded_rustflags = [
        "-C",
        "passes=lower-atomic",
        "-C",
        &format!("link-arg=-Ttext={USER_TEXT_START:#010x}"),
        "-C",
        "link-arg=--fatal-warnings",
        "-C",
        "panic=abort",
        "--cfg",
        "getrandom_backend=\"custom\"",
    ]
    .join("\x1f");

    let mut cmd = Command::new("docker");
    cmd.arg("run")
        .arg("--rm")
        .arg("--volume")
        .arg(&volume)
        .arg("--workdir")
        .arg("/src")
        .arg("--env")
        .arg("RISC0_FEATURE_bigint2=")
        .arg("--env")
        .arg("CC_riscv32im_risc0_zkvm_elf=/root/.risc0/cpp/bin/riscv32-unknown-elf-gcc")
        .arg("--env")
        .arg("CFLAGS_riscv32im_risc0_zkvm_elf=-march=rv32im -nostdlib")
        .arg("--env")
        .arg(format!("CARGO_ENCODED_RUSTFLAGS={encoded_rustflags}"))
        .arg(&image)
        .arg("-c")
        .arg(&docker_command);

    let status = cmd
        .status()
        .unwrap_or_else(|err| panic!("failed running docker {} for guest build: {err}", subcommand));
    if !status.success() {
        panic!("docker {} failed for guest build", subcommand);
    }
}

fn normalize_output_ownership(docker_root: &Path, target_dir_rel: &str, use_podman: bool) {
    let target_dir = docker_root.join(target_dir_rel);

    if use_podman {
        // Rootless Podman runs containers in a user namespace where the container's UID 0
        // maps to a sub-UID on the host (e.g. 166536). Files written by the container are
        // owned by that sub-UID. `podman unshare` enters the user namespace where UID 0 maps
        // back to the real host user, so `chown -R 0:0` restores host-user ownership.
        let status = Command::new("podman")
            .arg("unshare")
            .arg("chown")
            .arg("-R")
            .arg("0:0")
            .arg(&target_dir)
            .status()
            .expect("failed running podman unshare chown for guest output");
        if !status.success() {
            panic!("podman unshare chown failed for guest output");
        }
    } else {
        let uid = current_id("-u");
        let gid = current_id("-g");
        let image = format!("risczero/risc0-guest-builder:{}", docker_tag());
        let volume = format!("{}:/src", docker_root.display());
        let chown_cmd = format!("chown -R {uid}:{gid} {target_dir_rel}");

        let status = Command::new("docker")
            .arg("run")
            .arg("--rm")
            .arg("--volume")
            .arg(&volume)
            .arg("--workdir")
            .arg("/src")
            .arg(&image)
            .arg("-c")
            .arg(&chown_cmd)
            .status()
            .expect("failed running docker chown for guest output");
        if !status.success() {
            panic!("docker chown failed for guest output");
        }
    }
}

/// Validates that a container runtime is available and returns whether it is podman.
fn detect_container_runtime() -> bool {
    let output = Command::new("docker")
        .arg("--version")
        .output()
        .expect("failed to run `docker --version` — is docker or podman installed?");
    if !output.status.success() {
        panic!("`docker --version` failed");
    }
    String::from_utf8_lossy(&output.stdout)
        .to_lowercase()
        .contains("podman")
}

fn docker_tag() -> String {
    env::var("RISC0_DOCKER_CONTAINER_TAG").unwrap_or_else(|_| DEFAULT_DOCKER_TAG.to_string())
}

fn current_id(flag: &str) -> String {
    let output = Command::new("id")
        .arg(flag)
        .output()
        .unwrap_or_else(|err| panic!("failed running `id {flag}`: {err}"));
    if !output.status.success() {
        panic!("`id {flag}` failed");
    }
    String::from_utf8(output.stdout)
        .expect("`id` output is not utf8")
        .trim()
        .to_string()
}

fn get_out_dir() -> PathBuf {
    if let Some(target_dir) = env::var_os("CARGO_TARGET_DIR").map(PathBuf::from) {
        if target_dir.is_absolute() {
            return target_dir.join("riscv-guest");
        }
    }

    let mut dir: PathBuf = env::var_os("OUT_DIR").expect("OUT_DIR is not set").into();
    loop {
        if dir.join(".rustc_info.json").exists()
            || dir.join("CACHEDIR.TAG").exists()
            || dir.file_name() == Some(OsStr::new("target"))
                && dir
                    .parent()
                    .is_some_and(|parent| parent.join("Cargo.toml").exists())
        {
            return dir.join("riscv-guest");
        }
        if dir.pop() {
            continue;
        }
        panic!("cannot find cargo target dir location");
    }
}
