use crate::{engine::digest_file, process};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const REQUIREMENTS: &str = "test \"$(uname -s)\" = Linux && test \"$(uname -m)\" = x86_64 || exit 1; for tool in make gcc tar xz bzip2 sha256sum find cp nproc; do command -v \"$tool\" >/dev/null || exit 1; done";

fn remote_host() -> Result<Option<String>> {
    let Some(host) = std::env::var_os("FORGE_BUSYBOX_SSH") else {
        return Ok(None);
    };
    let host = host
        .into_string()
        .map_err(|_| anyhow::anyhow!("FORGE_BUSYBOX_SSH must be UTF-8"))?;
    ensure!(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
        "FORGE_BUSYBOX_SSH must be an SSH alias or user@hostname"
    );
    Ok(Some(host))
}

fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host]);
    command
}

pub fn check_host() -> Result<()> {
    let host = remote_host()?;
    let mut command = if let Some(host) = &host {
        ssh(host)
    } else {
        ensure!(
            cfg!(all(target_os = "linux", target_arch = "x86_64")),
            "BusyBox needs a Linux x86_64 builder; set FORGE_BUSYBOX_SSH to an SSH alias (with ssh/scp installed locally)"
        );
        let mut command = Command::new("sh");
        command.arg("-c");
        command
    };
    let result = command
        .arg(REQUIREMENTS)
        .output()
        .context("checking BusyBox builder")?;
    ensure!(
        result.status.success(),
        "BusyBox builder requires Linux x86_64 with make, gcc, tar, xz, bzip2, sha256sum, find, cp and nproc: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    println!(
        "[ok] BusyBox builder: {}",
        host.as_deref().unwrap_or("local Linux x86_64")
    );
    Ok(())
}

pub fn build(source: &Path, toolchain: &Path, config: &Path, out: &Path) -> Result<PathBuf> {
    check_host()?;
    let source_hash = digest_file(source)?;
    let toolchain_hash = digest_file(toolchain)?;
    let work = out.join(format!("{}-{}", &source_hash[..16], &toolchain_hash[..16]));
    fs::create_dir_all(&work)?;
    let work = crate::config::absolute(&work)?;
    for (input, name, hash) in [
        (source, "busybox.tar.bz2", &source_hash),
        (toolchain, "toolchain.tar.xz", &toolchain_hash),
    ] {
        let destination = work.join(name);
        // Reflinks/hardlinks are not assumed across filesystems.
        if !destination.is_file() || digest_file(&destination)? != *hash {
            fs::copy(input, destination)?;
        }
    }
    fs::write(
        work.join("busybox.config"),
        fs::read_to_string(config)?.replace("\r\n", "\n"),
    )?;
    if let Some(host) = remote_host()? {
        let output = ssh(&host)
            .arg("mktemp -d /tmp/sdsat-forge-busybox.XXXXXXXX")
            .output()?;
        ensure!(
            output.status.success(),
            "creating BusyBox build directory: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let remote = String::from_utf8(output.stdout)?.trim().to_owned();
        let suffix = remote
            .strip_prefix("/tmp/sdsat-forge-busybox.")
            .context("unexpected remote build directory")?;
        ensure!(
            suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid remote build directory"
        );
        // Only this freshly created and validated directory is removed.
        let result = (|| -> Result<()> {
            process::logged(
                Command::new("scp")
                    .args([
                        "-B",
                        "-o",
                        "ConnectTimeout=10",
                        "busybox.tar.bz2",
                        "toolchain.tar.xz",
                        "busybox.config",
                        &format!("{host}:{remote}/"),
                    ])
                    .current_dir(&work),
                &out.join("upload.log"),
            )?;
            Builder {
                host: Some(&host),
                directory: &remote,
            }
            .build(out, &source_hash, &toolchain_hash)?;
            process::logged(
                Command::new("scp")
                    .args([
                        "-B",
                        "-o",
                        "ConnectTimeout=10",
                        &format!("{host}:{remote}/busybox"),
                        &format!("{host}:{remote}/busybox.config.final"),
                        ".",
                    ])
                    .current_dir(&work),
                &out.join("download.log"),
            )
        })();
        let cleanup = ssh(&host)
            .arg(format!("rm -rf -- {}", shell_quote(&remote)))
            .status();
        if !cleanup.is_ok_and(|status| status.success()) {
            eprintln!("[forge] could not remove remote build directory {host}:{remote}");
        }
        result?;
    } else {
        Builder {
            host: None,
            directory: work.to_str().context("BusyBox build path must be UTF-8")?,
        }
        .build(out, &source_hash, &toolchain_hash)?;
    }
    let binary = work.join("busybox");
    ensure!(
        binary.is_file(),
        "BusyBox build did not produce an executable"
    );
    Ok(binary)
}

struct Builder<'a> {
    host: Option<&'a str>,
    directory: &'a str,
}

impl Builder<'_> {
    fn command(&self, program: &str, args: &[&str]) -> Command {
        if let Some(host) = self.host {
            let mut command = ssh(host);
            let invocation = std::iter::once(program)
                .chain(args.iter().copied())
                .map(shell_quote)
                .collect::<Vec<_>>()
                .join(" ");
            command.arg(format!(
                "cd {} && LC_ALL=C {invocation}",
                shell_quote(self.directory)
            ));
            command
        } else {
            let mut command = Command::new(program);
            command
                .args(args)
                .current_dir(self.directory)
                .env("LC_ALL", "C");
            command
        }
    }

    fn build(&self, out: &Path, source_hash: &str, toolchain_hash: &str) -> Result<()> {
        // Cargo inherits this setting; translate it for the C build as well.
        let jobs = match std::env::var("CARGO_BUILD_JOBS") {
            Ok(value) => value
                .parse::<usize>()
                .context("BusyBox requires CARGO_BUILD_JOBS to be a positive integer")?,
            Err(std::env::VarError::NotPresent) => {
                let log = out.join("parallelism.log");
                process::logged(&mut self.command("nproc", &[]), &log)?;
                fs::read_to_string(log)?
                    .trim()
                    .parse::<usize>()
                    .context("reading BusyBox builder CPU count")?
            }
            Err(error) => return Err(error).context("reading CARGO_BUILD_JOBS"),
        };
        ensure!(jobs > 0, "BusyBox parallel jobs must be positive");
        println!("[forge] BusyBox parallel jobs: {jobs}");
        let checksum_log = out.join("checksums.log");
        process::logged(
            &mut self.command("sha256sum", &["busybox.tar.bz2", "toolchain.tar.xz"]),
            &checksum_log,
        )?;
        let checksums = fs::read_to_string(checksum_log)?;
        for (name, expected) in [
            ("busybox.tar.bz2", source_hash),
            ("toolchain.tar.xz", toolchain_hash),
        ] {
            ensure!(
                checksums.lines().any(|line| {
                    let mut fields = line.split_whitespace();
                    fields.next() == Some(expected)
                        && fields.next() == Some(name)
                        && fields.next().is_none()
                }),
                "BusyBox builder archive checksum mismatch: {name}"
            );
        }
        process::logged(
            &mut self.command("mkdir", &["-p", "source", "toolchain", "build"]),
            &out.join("prepare.log"),
        )?;
        for (archive, directory, flag) in [
            ("busybox.tar.bz2", "source", "-xjf"),
            ("toolchain.tar.xz", "toolchain", "-xJf"),
        ] {
            process::logged(
                &mut self.command(
                    "tar",
                    &[flag, archive, "--strip-components=1", "-C", directory],
                ),
                &out.join(format!("extract-{directory}.log")),
            )?;
        }
        let toolchain_bin = format!("{}/toolchain/bin", self.directory);
        let toolchain_log = out.join("find-toolchain.log");
        process::logged(
            &mut self.command(
                "find",
                &[
                    "-L",
                    &toolchain_bin,
                    "-maxdepth",
                    "1",
                    "-name",
                    "riscv64*-gcc",
                    "-type",
                    "f",
                    "-print",
                    "-quit",
                ],
            ),
            &toolchain_log,
        )?;
        let found = fs::read_to_string(toolchain_log)?;
        let compiler = found.trim();
        ensure!(
            compiler.starts_with(&format!("{toolchain_bin}/riscv64"))
                && compiler.ends_with("-gcc")
                && !compiler.contains(['\n', '\r']),
            "Bootlin RISC-V 64 GCC is missing from toolchain/bin"
        );
        let output_arg = format!("O={}/build", self.directory);
        let cross_arg = format!("CROSS_COMPILE={}", compiler.strip_suffix("gcc").unwrap());
        let config_arg = format!("KBUILD_DEFCONFIG={}/busybox.config", self.directory);
        let common = ["-C", "source", &output_arg, "ARCH=riscv", &cross_arg];
        let mut configure = common.to_vec();
        configure.extend([&config_arg, "defconfig"]);
        process::logged(
            &mut self.command("make", &configure),
            &out.join("configure.log"),
        )?;
        let jobs_arg = format!("-j{jobs}");
        let mut compile = common.to_vec();
        compile.extend([
            "EXTRA_CFLAGS=-march=rv64gc -mabi=lp64d -Os",
            "EXTRA_LDFLAGS=-march=rv64gc -mabi=lp64d",
            &jobs_arg,
        ]);
        process::logged(&mut self.command("make", &compile), &out.join("build.log"))?;
        process::logged(
            &mut self.command("cp", &["build/busybox", "busybox"]),
            &out.join("export-binary.log"),
        )?;
        process::logged(
            &mut self.command("cp", &["build/.config", "busybox.config.final"]),
            &out.join("export-config.log"),
        )
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
