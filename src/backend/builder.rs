use crate::{engine::digest_file, process};
use anyhow::{Context, Result, ensure};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const TOOLS: &[&str] = &[
    "make",
    "gcc",
    "tar",
    "xz",
    "bzip2",
    "sha256sum",
    "find",
    "cp",
    "nproc",
];

pub enum Config {
    Defconfig,
    Olddefconfig,
}

pub struct Kbuild {
    pub name: &'static str,
    pub tools: &'static [&'static str],
    pub config: Config,
    pub args: &'static [&'static str],
    pub artifact: &'static str,
}

fn remote_host() -> Result<Option<String>> {
    let Some(host) =
        std::env::var_os("FORGE_BUILD_SSH").or_else(|| std::env::var_os("FORGE_BUSYBOX_SSH"))
    else {
        return Ok(None);
    };
    let host = host
        .into_string()
        .map_err(|_| anyhow::anyhow!("build SSH host must be UTF-8"))?;
    ensure!(
        !host.is_empty()
            && !host.starts_with('-')
            && host
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
        "FORGE_BUILD_SSH must be an SSH alias or user@hostname"
    );
    Ok(Some(host))
}

fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host]);
    command
}

impl Kbuild {
    pub fn check_host(&self) -> Result<()> {
        let host = remote_host()?;
        let mut command = if let Some(host) = &host {
            ssh(host)
        } else {
            ensure!(
                cfg!(all(target_os = "linux", target_arch = "x86_64")),
                "{} needs a Linux x86_64 builder for the configured Bootlin toolchain; set FORGE_BUILD_SSH to an SSH alias (with ssh/scp installed locally)",
                self.name
            );
            let mut command = Command::new("sh");
            command.arg("-c");
            command
        };
        let tools = TOOLS
            .iter()
            .chain(self.tools)
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        let script = format!(
            "test \"$(uname -s)\" = Linux && test \"$(uname -m)\" = x86_64 || exit 1; for tool in {tools}; do command -v \"$tool\" >/dev/null || {{ echo \"missing tool: $tool\" >&2; exit 1; }}; done"
        );
        let result = command
            .arg(script)
            .output()
            .with_context(|| format!("checking {} builder", self.name))?;
        ensure!(
            result.status.success(),
            "{} builder requires Linux x86_64 with {tools}: {}",
            self.name,
            String::from_utf8_lossy(&result.stderr).trim()
        );
        println!(
            "[ok] {} builder: {}",
            self.name,
            host.as_deref().unwrap_or("local Linux x86_64")
        );
        Ok(())
    }

    pub fn build(
        &self,
        source: &Path,
        toolchain: &Path,
        config: &Path,
        out: &Path,
    ) -> Result<PathBuf> {
        self.check_host()?;
        let source_hash = digest_file(source)?;
        let toolchain_hash = digest_file(toolchain)?;
        let work = out.join(format!("{}-{}", &source_hash[..16], &toolchain_hash[..16]));
        fs::create_dir_all(&work)?;
        let work = crate::config::absolute(&work)?;
        for (input, name, hash) in [
            (source, "source.tar", &source_hash),
            (toolchain, "toolchain.tar.xz", &toolchain_hash),
        ] {
            let destination = work.join(name);
            if !destination.is_file() || digest_file(&destination)? != *hash {
                fs::copy(input, destination)?;
            }
        }
        fs::write(
            work.join("build.config"),
            fs::read_to_string(config)?.replace("\r\n", "\n"),
        )?;
        let config_hash = digest_file(&work.join("build.config"))?;
        let checksums = [
            ("source.tar", source_hash.as_str()),
            ("toolchain.tar.xz", toolchain_hash.as_str()),
            ("build.config", config_hash.as_str()),
        ];
        let artifact = Path::new(self.artifact)
            .file_name()
            .context("Kbuild artifact has no filename")?
            .to_str()
            .context("Kbuild artifact filename must be UTF-8")?;
        let config_final = format!("{}.config.final", self.name.to_ascii_lowercase());
        if let Some(host) = remote_host()? {
            let output = ssh(&host)
                .arg("mktemp -d /tmp/sdsat-forge-build.XXXXXXXX")
                .output()?;
            ensure!(
                output.status.success(),
                "creating build directory: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let remote = String::from_utf8(output.stdout)?.trim().to_owned();
            let suffix = remote
                .strip_prefix("/tmp/sdsat-forge-build.")
                .context("unexpected remote build directory")?;
            ensure!(
                suffix.len() == 8 && suffix.bytes().all(|b| b.is_ascii_alphanumeric()),
                "invalid remote build directory"
            );
            let result = (|| -> Result<()> {
                process::logged(
                    Command::new("scp")
                        .args([
                            "-B",
                            "-o",
                            "ConnectTimeout=10",
                            "source.tar",
                            "toolchain.tar.xz",
                            "build.config",
                            &format!("{host}:{remote}/"),
                        ])
                        .current_dir(&work),
                    &out.join("upload.log"),
                )?;
                Builder {
                    host: Some(&host),
                    directory: &remote,
                    out,
                }
                .build(self, &checksums, artifact, &config_final)?;
                process::logged(
                    Command::new("scp")
                        .args([
                            "-B",
                            "-o",
                            "ConnectTimeout=10",
                            &format!("{host}:{remote}/{artifact}"),
                            &format!("{host}:{remote}/{config_final}"),
                            ".",
                        ])
                        .current_dir(&work),
                    &out.join("download.log"),
                )
            })();
            // Only this freshly created and validated directory is removed.
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
                directory: work.to_str().context("build path must be UTF-8")?,
                out,
            }
            .build(self, &checksums, artifact, &config_final)?;
        }
        let binary = work.join(artifact);
        ensure!(
            binary.is_file(),
            "{} build did not produce {artifact}",
            self.name
        );
        Ok(binary)
    }
}

struct Builder<'a> {
    host: Option<&'a str>,
    directory: &'a str,
    out: &'a Path,
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

    fn run(&self, program: &str, args: &[&str], log: &str) -> Result<()> {
        process::logged(
            &mut self.command(program, args),
            &self.out.join(format!("{log}.log")),
        )
    }

    fn build(
        &self,
        package: &Kbuild,
        checksums: &[(&str, &str)],
        artifact: &str,
        config_final: &str,
    ) -> Result<()> {
        // Cargo inherits this setting; translate it for Kbuild as well.
        let jobs = match std::env::var("CARGO_BUILD_JOBS") {
            Ok(value) => value
                .parse::<usize>()
                .context("Kbuild requires CARGO_BUILD_JOBS to be a positive integer")?,
            Err(std::env::VarError::NotPresent) => {
                self.run("nproc", &[], "parallelism")?;
                fs::read_to_string(self.out.join("parallelism.log"))?
                    .trim()
                    .parse::<usize>()
                    .context("reading builder CPU count")?
            }
            Err(error) => return Err(error).context("reading CARGO_BUILD_JOBS"),
        };
        ensure!(jobs > 0, "Kbuild parallel jobs must be positive");
        println!("[forge] {} parallel jobs: {jobs}", package.name);
        self.run(
            "sha256sum",
            &checksums.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            "checksums",
        )?;
        let actual = fs::read_to_string(self.out.join("checksums.log"))?;
        for (name, expected) in checksums {
            ensure!(
                actual.lines().any(|line| {
                    let mut fields = line.split_whitespace();
                    fields.next() == Some(*expected)
                        && fields.next() == Some(*name)
                        && fields.next().is_none()
                }),
                "{} builder input checksum mismatch: {name}",
                package.name
            );
        }
        self.run("mkdir", &["-p", "source", "toolchain", "build"], "prepare")?;
        for (archive, directory) in [("source.tar", "source"), ("toolchain.tar.xz", "toolchain")] {
            self.run(
                "tar",
                &["-xf", archive, "--strip-components=1", "-C", directory],
                &format!("extract-{directory}"),
            )?;
        }
        let toolchain_bin = format!("{}/toolchain/bin", self.directory);
        self.run(
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
            "find-toolchain",
        )?;
        let found = fs::read_to_string(self.out.join("find-toolchain.log"))?;
        let compiler = found.trim();
        ensure!(
            compiler.starts_with(&format!("{toolchain_bin}/riscv64"))
                && compiler.ends_with("-gcc")
                && !compiler.contains(['\n', '\r']),
            "Bootlin RISC-V 64 GCC is missing from toolchain/bin"
        );
        let output_arg = format!("O={}/build", self.directory);
        let cross_arg = format!("CROSS_COMPILE={}", compiler.strip_suffix("gcc").unwrap());
        let common = ["-C", "source", &output_arg, "ARCH=riscv", &cross_arg];
        let config_arg = format!("KBUILD_DEFCONFIG={}/build.config", self.directory);
        let mut configure = common.to_vec();
        match package.config {
            Config::Defconfig => configure.extend([config_arg.as_str(), "defconfig"]),
            Config::Olddefconfig => {
                self.run("cp", &["build.config", "build/.config"], "prepare-config")?;
                configure.push("olddefconfig");
            }
        }
        self.run("make", &configure, "configure")?;
        let jobs_arg = format!("-j{jobs}");
        let mut compile = common.to_vec();
        compile.push(&jobs_arg);
        compile.extend(package.args);
        self.run("make", &compile, "build")?;
        self.run(
            "cp",
            &[&format!("build/{}", package.artifact), artifact],
            "export-binary",
        )?;
        self.run("cp", &["build/.config", config_final], "export-config")
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
