use crate::{config::Recipe, engine::digest_file, process};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

const TOOLS: &[&str] = &[
    "sh",
    "tar",
    "sha256sum",
    "cp",
    "rm",
    "mkdir",
    "nproc",
    "uname",
    "make",
    "find",
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigMode {
    Defconfig,
    Olddefconfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KbuildConfig {
    #[serde(default)]
    pub tools: Vec<String>,
    pub config: ConfigMode,
    pub arch: String,
    pub compiler_pattern: String,
    pub compiler_suffix: String,
    pub host_os: Option<String>,
    pub host_arch: Option<String>,
}

fn relative_path(path: &str) -> Result<()> {
    ensure!(
        path.split('/').all(|part| {
            !part.is_empty()
                && part != "."
                && part != ".."
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        }),
        "invalid builder relative path: {path}"
    );
    Ok(())
}

fn ssh_builder_hint() -> &'static str {
    if cfg!(windows) {
        "To use a Linux SSH builder, run in this PowerShell session:\n  \
         $env:FORGE_BUILD_SSH = \"user@linux-host\"\n\
         An SSH config Host alias is also accepted. The host must be reachable \
         without interactive prompts (ssh -o BatchMode=yes)."
    } else {
        "To use a Linux SSH builder, run in this shell session:\n  \
         export FORGE_BUILD_SSH='user@linux-host'\n\
         An SSH config Host alias is also accepted. The host must be reachable \
         without interactive prompts (ssh -o BatchMode=yes)."
    }
}

fn remote_host() -> Result<Option<String>> {
    let Some(host) = std::env::var_os("FORGE_BUILD_SSH") else {
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
        "FORGE_BUILD_SSH must be an SSH alias or user@hostname.\n{}",
        ssh_builder_hint()
    );
    Ok(Some(host))
}

fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", host]);
    command
}

fn token(value: &str, kind: &str) -> Result<()> {
    ensure!(
        !value.is_empty()
            && !value.starts_with('-')
            && value != "."
            && value != ".."
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte)),
        "invalid builder {kind}: {value:?}"
    );
    Ok(())
}

fn artifacts(recipe: &Recipe) -> Result<BTreeMap<String, String>> {
    let output = recipe
        .build_output
        .as_ref()
        .context("archive builder requires build_output")?;
    relative_path(output)?;
    let mut artifacts = recipe.artifacts.clone();
    if let Some(name) = artifacts.insert(output.clone(), recipe.artifact.clone()) {
        ensure!(
            name == recipe.artifact,
            "build_output {output} has conflicting artifact names"
        );
    }
    let mut destinations = BTreeSet::new();
    for (path, name) in &artifacts {
        relative_path(path)?;
        crate::config::validate_filename(name)?;
        ensure!(
            destinations.insert(name.clone()),
            "duplicate builder artifact name: {name}"
        );
    }
    Ok(artifacts)
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    let kbuild = recipe.kbuild.as_ref().context("missing kbuild settings")?;
    crate::config::validate_name(&recipe.name)?;
    ensure!(recipe.extract, "archive builders require extract = true");
    ensure!(recipe.config.is_some(), "archive builder requires config");
    ensure!(
        recipe.toolchain.is_some(),
        "archive builder requires toolchain"
    );
    token(&kbuild.arch, "architecture")?;
    token(&kbuild.compiler_suffix, "compiler suffix")?;
    ensure!(
        !kbuild.compiler_pattern.is_empty()
            && !kbuild.compiler_pattern.starts_with('-')
            && kbuild.compiler_pattern.ends_with(&kbuild.compiler_suffix)
            && kbuild
                .compiler_pattern
                .bytes()
                .all(|byte| { byte.is_ascii_alphanumeric() || b"._+-*?".contains(&byte) }),
        "Kbuild compiler_pattern must be a filename glob ending in compiler_suffix (only '*' and '?' wildcards): {:?}",
        kbuild.compiler_pattern
    );
    for tool in &kbuild.tools {
        token(tool, "required tool")?;
    }
    for value in [&kbuild.host_os, &kbuild.host_arch].into_iter().flatten() {
        token(value, "host requirement")?;
    }
    artifacts(recipe)?;
    Ok(())
}

fn host_script(kbuild: &KbuildConfig, patched: bool) -> String {
    let mut script = String::new();
    for (flag, expected) in [("-s", &kbuild.host_os), ("-m", &kbuild.host_arch)] {
        if let Some(expected) = expected {
            script.push_str(&format!(
                "test \"$(uname {flag})\" = {} || {{ echo {} >&2; exit 1; }}; ",
                shell_quote(expected),
                shell_quote(&format!("builder requires uname {flag} = {expected}"))
            ));
        }
    }
    let tools = TOOLS
        .iter()
        .copied()
        .chain(kbuild.tools.iter().map(String::as_str))
        .chain(patched.then_some("git"))
        .map(shell_quote)
        .collect::<Vec<_>>()
        .join(" ");
    script.push_str(&format!(
        "for tool in {tools}; do command -v \"$tool\" >/dev/null || {{ echo \"missing tool: $tool\" >&2; exit 1; }}; done"
    ));
    script
}

pub fn check_host(recipe: &Recipe) -> Result<()> {
    validate(recipe)?;
    let kbuild = recipe.kbuild.as_ref().context("missing kbuild settings")?;
    let host = remote_host()?;
    let mut command = if let Some(host) = &host {
        ssh(host)
    } else {
        let mut command = Command::new("sh");
        command.arg("-c");
        command
    };
    let result = command
        .arg(host_script(kbuild, !recipe.patches.is_empty()))
        .output()
        .with_context(|| format!("checking {} builder\n{}", recipe.name, ssh_builder_hint()))?;
    ensure!(
        result.status.success(),
        "{} builder requirements were not met: {}\n{}",
        recipe.name,
        String::from_utf8_lossy(&result.stderr).trim(),
        ssh_builder_hint()
    );
    println!(
        "[ok] {} builder: {}",
        recipe.name,
        host.as_deref().unwrap_or("local")
    );
    Ok(())
}

pub fn build(
    recipe: &Recipe,
    source: &Path,
    toolchain: &Path,
    config: &Path,
    patches: &[PathBuf],
    out: &Path,
) -> Result<crate::backend::BuildOutput> {
    validate(recipe)?;
    ensure!(
        patches.len() == recipe.patches.len(),
        "{} builder patch inputs do not match the recipe",
        recipe.name
    );
    let artifacts = artifacts(recipe)?;
    let destinations = artifacts.values().collect::<BTreeSet<_>>();
    check_host(recipe)?;
    let source_hash = digest_file(source)?;
    let toolchain_hash = digest_file(toolchain)?;
    let patch_hashes = patches
        .iter()
        .map(|patch| digest_file(patch))
        .collect::<Result<Vec<_>>>()?;
    let mut work_name = format!("{}-{}", &source_hash[..16], &toolchain_hash[..16]);
    if !patch_hashes.is_empty() {
        let patch_key = format!("{:x}", Sha256::digest(patch_hashes.concat()));
        work_name.push('-');
        work_name.push_str(&patch_key[..16]);
    }
    let work = crate::config::absolute(out)?.join(work_name);
    fs::create_dir_all(&work)?;
    ensure!(
        !fs::symlink_metadata(&work)?.file_type().is_symlink(),
        "builder work directory cannot be a symlink: {}",
        work.display()
    );
    let exported = work.join("artifacts");
    fs::create_dir_all(&exported)?;
    let mut checksums = Vec::new();
    for (input, name, hash) in [
        (source, "source.tar", source_hash),
        (toolchain, "toolchain.tar.xz", toolchain_hash),
    ] {
        let destination = work.join(name);
        if !destination.is_file() || digest_file(&destination)? != hash {
            fs::copy(input, destination)?;
        }
        checksums.push((name.to_owned(), hash));
    }
    for (index, (patch, hash)) in patches.iter().zip(patch_hashes).enumerate() {
        let name = format!("patch-{index}.patch");
        fs::copy(patch, work.join(&name))?;
        checksums.push((name, hash));
    }
    fs::write(
        work.join("build.config"),
        fs::read_to_string(config)?.replace("\r\n", "\n"),
    )?;
    checksums.push((
        "build.config".to_owned(),
        digest_file(&work.join("build.config"))?,
    ));
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
                    .args(["-B", "-o", "ConnectTimeout=10"])
                    .args(checksums.iter().map(|(name, _)| name))
                    .arg(format!("{host}:{remote}/"))
                    .current_dir(&work),
                &out.join("upload.log"),
            )?;
            Builder {
                host: Some(&host),
                directory: &remote,
                out,
            }
            .build(recipe, &checksums, &artifacts)?;
            process::logged(
                Command::new("scp")
                    .args(["-B", "-o", "ConnectTimeout=10"])
                    .args(
                        destinations
                            .iter()
                            .map(|name| format!("{host}:{remote}/artifacts/{name}")),
                    )
                    .arg(".")
                    .current_dir(&exported),
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
        .build(recipe, &checksums, &artifacts)?;
    }
    for name in artifacts.values() {
        ensure!(
            exported.join(name).is_file(),
            "{} build did not produce {name}",
            recipe.name
        );
    }
    let primary = exported.join(&recipe.artifact);
    let additional = recipe
        .artifacts
        .values()
        .map(|name| (name.clone(), exported.join(name)))
        .collect();
    Ok(crate::backend::BuildOutput {
        primary,
        additional,
    })
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
        recipe: &Recipe,
        checksums: &[(String, String)],
        artifacts: &BTreeMap<String, String>,
    ) -> Result<()> {
        // Preserve the top-level build's parallelism for package recipes.
        let jobs = match std::env::var("CARGO_BUILD_JOBS") {
            Ok(value) => value
                .parse::<usize>()
                .context("builder requires CARGO_BUILD_JOBS to be a positive integer")?,
            Err(std::env::VarError::NotPresent) => {
                self.run("nproc", &[], "parallelism")?;
                fs::read_to_string(self.out.join("parallelism.log"))?
                    .trim()
                    .parse::<usize>()
                    .context("reading builder CPU count")?
            }
            Err(error) => return Err(error).context("reading CARGO_BUILD_JOBS"),
        };
        ensure!(jobs > 0, "builder parallel jobs must be positive");
        println!("[forge] {} parallel jobs: {jobs}", recipe.name);
        self.run(
            "sha256sum",
            &checksums
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            "checksums",
        )?;
        let actual = fs::read_to_string(self.out.join("checksums.log"))?;
        for (name, expected) in checksums {
            ensure!(
                actual.lines().any(|line| {
                    let mut fields = line.split_whitespace();
                    fields.next() == Some(expected.as_str())
                        && fields.next() == Some(name.as_str())
                        && fields.next().is_none()
                }),
                "{} builder input checksum mismatch: {name}",
                recipe.name
            );
        }
        // These fixed children belong to the validated work directory; rm removes
        // child symlinks themselves without following them.
        self.run(
            "rm",
            &["-rf", "--", "source", "toolchain", "build", "artifacts"],
            "clean",
        )?;
        self.run(
            "mkdir",
            &["-p", "source", "toolchain", "build", "artifacts"],
            "prepare",
        )?;
        for (archive, directory) in [("source.tar", "source"), ("toolchain.tar.xz", "toolchain")] {
            self.run(
                "tar",
                &["-xf", archive, "--strip-components=1", "-C", directory],
                &format!("extract-{directory}"),
            )?;
        }
        for index in 0..recipe.patches.len() {
            for check in [true, false] {
                // Do not discover the enclosing Forge repository: git apply
                // otherwise silently skips paths outside this nested source tree.
                let script = format!(
                    "cd source && GIT_CEILING_DIRECTORIES={} git apply --verbose {} -- ../patch-{index}.patch",
                    shell_quote(self.directory),
                    if check { "--check" } else { "" },
                );
                self.run(
                    "sh",
                    &["-c", &script],
                    &format!("patch-{index}-{}", if check { "check" } else { "apply" }),
                )?;
            }
        }
        let build_dir = build_directory(recipe.in_tree);
        self.kbuild(recipe, jobs)?;
        for (path, name) in artifacts {
            self.run(
                "cp",
                &[&format!("{build_dir}/{path}"), &format!("artifacts/{name}")],
                "export-artifact",
            )?;
        }
        Ok(())
    }

    fn kbuild(&self, recipe: &Recipe, jobs: usize) -> Result<()> {
        let kbuild = recipe.kbuild.as_ref().context("missing kbuild settings")?;
        let toolchain_bin = format!("{}/toolchain/bin", self.directory);
        self.run(
            "find",
            &[
                "-L",
                &toolchain_bin,
                "-maxdepth",
                "1",
                "-name",
                &kbuild.compiler_pattern,
                "-type",
                "f",
                "-print",
                "-quit",
            ],
            "find-toolchain",
        )?;
        let found = fs::read_to_string(self.out.join("find-toolchain.log"))?;
        let prefix = compiler_prefix(kbuild, &toolchain_bin, &found)?;
        let common = make_args(kbuild, self.directory, prefix, recipe.in_tree);
        if matches!(kbuild.config, ConfigMode::Olddefconfig) {
            self.run(
                "cp",
                &[
                    "build.config",
                    &format!("{}/.config", build_directory(recipe.in_tree)),
                ],
                "prepare-config",
            )?;
        }
        let configure = configure_args(kbuild.config, common.clone(), self.directory);
        self.run(
            "make",
            &configure.iter().map(String::as_str).collect::<Vec<_>>(),
            "configure",
        )?;
        let mut compile = common;
        compile.push(format!("-j{jobs}"));
        compile.extend(recipe.args.iter().cloned());
        self.run(
            "make",
            &compile.iter().map(String::as_str).collect::<Vec<_>>(),
            "build",
        )
    }
}

fn build_directory(in_tree: bool) -> &'static str {
    if in_tree { "source" } else { "build" }
}

fn compiler_prefix<'a>(
    kbuild: &KbuildConfig,
    toolchain_bin: &str,
    found: &'a str,
) -> Result<&'a str> {
    let compiler = found.trim_end_matches(['\r', '\n']);
    let filename = compiler
        .strip_prefix(&format!("{toolchain_bin}/"))
        .context("compiler must be inside toolchain/bin")?;
    token(filename, "compiler filename")?;
    compiler
        .strip_suffix(&kbuild.compiler_suffix)
        .with_context(|| format!("compiler must end in {:?}", kbuild.compiler_suffix))
}

fn make_args(
    kbuild: &KbuildConfig,
    directory: &str,
    cross_prefix: &str,
    in_tree: bool,
) -> Vec<String> {
    let mut args = vec![
        "-C".into(),
        "source".into(),
        format!("ARCH={}", kbuild.arch),
        format!("CROSS_COMPILE={cross_prefix}"),
    ];
    if !in_tree {
        args.push(format!("O={directory}/{}", build_directory(in_tree)));
    }
    args
}

fn configure_args(mode: ConfigMode, mut common: Vec<String>, directory: &str) -> Vec<String> {
    match mode {
        ConfigMode::Defconfig => common.extend([
            format!("KBUILD_DEFCONFIG={directory}/build.config"),
            "defconfig".into(),
        ]),
        ConfigMode::Olddefconfig => common.push("olddefconfig".into()),
    }
    common
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
