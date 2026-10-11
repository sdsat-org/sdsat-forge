//! Build the pinned Asterinas RISC-V kernel using its OSDK source templates.
//! Generated upstream files retain their upstream license.

use crate::{config, process};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const TARGET: &str = "riscv64imac-unknown-none-elf";
const BINARY: &str = "asterinas-osdk-bin";
const RUSTFLAGS: &[&str] = &[
    "-Clink-arg=-Triscv64.ld",
    "-Crelocation-model=static",
    "-Crelro-level=off",
    "-Cforce-unwind-tables=yes",
    "--check-cfg=cfg(ktest)",
    "-Cno-redzone=y",
    "-Ctarget-feature=+zicbom",
];

pub fn build(source: &Path, vdso: &Path, out: &Path, toolchain: &str) -> Result<PathBuf> {
    let source = config::absolute(source)?;
    let vdso = config::absolute(vdso)?;
    ensure!(
        vdso.join("vdso_riscv64.so").is_file(),
        "missing RISC-V vDSO"
    );
    fs::create_dir_all(out)?;
    let out = config::absolute(out)?;
    let wrapper = out.join("wrapper");
    let source_lock = prepare_wrapper(&source, &wrapper)?;

    // Resolve the generated package without compiling, then reject changes to
    // upstream dependencies before executing the locked kernel build.
    let status = cargo(toolchain, &wrapper)
        .args([
            "metadata",
            "--format-version=1",
            "--filter-platform",
            TARGET,
        ])
        .stdout(Stdio::null())
        .status()
        .context("resolving Asterinas wrapper")?;
    ensure!(
        status.success(),
        "resolving Asterinas wrapper failed: {status}"
    );
    validate_external_lock(
        &source_lock,
        &fs::read_to_string(wrapper.join("Cargo.lock"))?,
    )?;

    let mut command = cargo(toolchain, &wrapper);
    command
        .args(["build", "--locked", "--release", "--target", TARGET])
        .arg("--target-dir")
        .arg(out.join("target"))
        .args([
            "-Zbuild-std=core,alloc,compiler_builtins",
            "-Zbuild-std-features=compiler-builtins-mem",
        ])
        .env("CARGO_ENCODED_RUSTFLAGS", RUSTFLAGS.join("\x1f"))
        .env("VDSO_LIBRARY_DIR", path_text(&vdso)?)
        .env("OSTD_TASK_STACK_SIZE_IN_PAGES", "16")
        .env("OSDK_BUILD_USERNAME", "sdsat-forge")
        .env("OSDK_BUILD_HOSTNAME", "forge-builder")
        .env("OSDK_BUILD_RUSTC", toolchain)
        .env("ASTER_BUILD_TIMESTAMP", "1970-01-01T00:00:00Z");
    process::logged(&mut command, &out.join("build.log"))?;
    let elf = out.join("target").join(TARGET).join("release").join(BINARY);
    ensure!(elf.is_file(), "Asterinas ELF missing: {}", elf.display());
    Ok(elf)
}

fn cargo(toolchain: &str, cwd: &Path) -> Command {
    let mut command = Command::new("cargo");
    process::isolate_rust(&mut command);
    command.arg(format!("+{toolchain}")).current_dir(cwd);
    command
}

fn prepare_wrapper(source: &Path, wrapper: &Path) -> Result<String> {
    let workspace: toml::Table = config::read(&source.join("Cargo.toml"))?;
    let kernel: toml::Value = config::read(&source.join("kernel/Cargo.toml"))?;
    let version = kernel["package"]["version"]
        .as_str()
        .context("missing Asterinas version")?;
    let templates = source.join("osdk/src/base_crate");
    let template = fs::read_to_string(templates.join("Cargo.toml.template"))?
        .replace("#NAME#", BINARY)
        .replace("#VERSION#", version);
    let mut manifest: toml::Table = toml::from_str(&template)?;
    let mut dependencies = toml::Table::new();
    for (name, relative) in [
        ("asterinas", "kernel"),
        ("ostd", "ostd"),
        ("osdk-frame-allocator", "osdk/deps/frame-allocator"),
        ("osdk-heap-allocator", "osdk/deps/heap-allocator"),
    ] {
        dependencies.insert(
            name.into(),
            toml::Value::Table(toml::Table::from_iter([(
                "path".into(),
                toml::Value::String(path_text(&source.join(relative))?),
            )])),
        );
    }
    // The system profile uses the kernel's own default features. No
    // feature forwarding or arbitrary kernel-name adaptation is needed.
    manifest.insert("dependencies".into(), dependencies.into());
    manifest.insert(
        "profile".into(),
        workspace
            .get("profile")
            .context("missing Asterinas profiles")?
            .clone(),
    );
    fs::create_dir_all(wrapper.join("src"))?;
    write_if_changed(
        &wrapper.join("Cargo.toml"),
        toml::to_string_pretty(&manifest)?.as_bytes(),
    )?;
    let main = fs::read_to_string(templates.join("main.rs.template"))?
        .replace("#TARGET_NAME#", "asterinas");
    write_if_changed(&wrapper.join("src/main.rs"), main.as_bytes())?;
    write_if_changed(
        &wrapper.join("riscv64.ld"),
        &fs::read(templates.join("riscv64.ld.template"))?,
    )?;

    // The working directory is reused. Always seed resolution from the selected
    // upstream lock so changing its source cannot retain an older dependency.
    let source_lock = fs::read_to_string(source.join("Cargo.lock"))?;
    fs::write(wrapper.join("Cargo.lock"), &source_lock)?;
    Ok(source_lock)
}

fn path_text(path: &Path) -> Result<String> {
    Ok(path
        .to_str()
        .context("Cargo paths must be UTF-8")?
        .replace('\\', "/"))
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<()> {
    if fs::read(path).ok().as_deref() != Some(contents) {
        fs::write(path, contents).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct CargoLock {
    package: Vec<LockedPackage>,
}

#[derive(Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
struct LockedPackage {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
}

fn external_packages(lock: &str) -> Result<BTreeSet<LockedPackage>> {
    Ok(toml::from_str::<CargoLock>(lock)?
        .package
        .into_iter()
        .filter(|package| package.source.is_some())
        .collect())
}

fn validate_external_lock(upstream: &str, generated: &str) -> Result<()> {
    let upstream = external_packages(upstream)?;
    let generated = external_packages(generated)?;
    ensure!(
        generated.is_subset(&upstream),
        "Asterinas wrapper changed upstream dependencies: {:?}; update the pinned source lock explicitly",
        generated.difference(&upstream).collect::<Vec<_>>()
    );
    Ok(())
}
