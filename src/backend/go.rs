//! Configured Go commands without package-name dispatch.

use crate::{backend, config::Recipe, engine::digest_file, process, source};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoConfig {
    pub cwd: Option<String>,
    pub compiler: Option<String>,
    pub host_os: Option<String>,
    pub host_arch: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn environment_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && !key.contains(['=', '\0']),
        "invalid Go environment key {key:?}"
    );
    Ok(())
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    let settings = recipe.go.as_ref().context("missing go settings")?;
    ensure!(!recipe.args.is_empty(), "Go package requires args");
    ensure!(
        recipe
            .build_output
            .as_ref()
            .is_some_and(|path| !path.is_empty()),
        "Go package requires build_output"
    );
    if let Some(cwd) = &settings.cwd {
        ensure!(!cwd.is_empty(), "Go working directory cannot be empty");
    }
    ensure!(
        recipe.toolchain.is_some() == settings.compiler.is_some(),
        "Go toolchain and go.compiler must be configured together"
    );
    if let Some(compiler) = &settings.compiler {
        ensure!(
            compiler.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"._+-".contains(&byte))
            }),
            "Go compiler must be a relative path inside the toolchain archive"
        );
    }
    for expected in [&settings.host_os, &settings.host_arch]
        .into_iter()
        .flatten()
    {
        ensure!(!expected.is_empty(), "Go host requirement cannot be empty");
    }
    for key in settings.env.keys() {
        environment_key(key)?;
    }
    Ok(())
}

fn host_hint() -> &'static str {
    if cfg!(windows) {
        "Use a Linux SSH builder; in PowerShell run: \
         $env:FORGE_BUILD_SSH = \"user@linux-host\" (an SSH config Host alias is also accepted). \
         SSH must connect without interactive prompts."
    } else {
        "Use a compatible SSH builder: export FORGE_BUILD_SSH='user@linux-host'. \
         SSH must connect without interactive prompts."
    }
}

pub fn check_host(recipe: &Recipe) -> Result<()> {
    validate(recipe)?;
    let settings = recipe.go.as_ref().context("missing go settings")?;
    ensure!(
        !cfg!(windows) || settings.compiler.is_none(),
        "Go archive toolchains require a Linux builder. {}",
        host_hint()
    );
    for (flag, expected) in [("-s", &settings.host_os), ("-m", &settings.host_arch)] {
        let Some(expected) = expected else { continue };
        let result = Command::new("uname")
            .arg(flag)
            .output()
            .with_context(|| format!("checking {} Go builder. {}", recipe.name, host_hint()))?;
        ensure!(
            result.status.success() && String::from_utf8_lossy(&result.stdout).trim() == expected,
            "{} Go builder requires uname {flag} = {expected}. {}",
            recipe.name,
            host_hint()
        );
    }
    Ok(())
}

fn compiler_path(directory: &Path, compiler: &str) -> Result<PathBuf> {
    let path = directory.join(compiler);
    ensure!(
        path.is_file() && path.canonicalize()?.starts_with(directory.canonicalize()?),
        "Go compiler must resolve to a file inside the toolchain: {}",
        path.display()
    );
    // Keep the configured name: compiler wrappers may dispatch using argv[0].
    Ok(path)
}

fn toolchain(archive: &Path, work: &Path, compiler: &str) -> Result<PathBuf> {
    const STAMP: &str = ".forge-toolchain-sha256";
    let checksum = digest_file(archive)?;
    let parent = work.join("toolchain");
    fs::create_dir_all(&parent)?;
    ensure!(
        fs::symlink_metadata(&parent)?.is_dir(),
        "Go toolchain cache cannot be a symlink"
    );
    let directory = parent.join(&checksum);
    if directory.try_exists()? {
        ensure!(
            fs::symlink_metadata(&directory)?.is_dir()
                && fs::read_to_string(directory.join(STAMP)).ok().as_deref() == Some(&checksum),
            "Go toolchain cache is incomplete or changed: {}",
            directory.display()
        );
    } else {
        let temporary = tempfile::tempdir_in(&parent)?;
        process::logged(
            Command::new("tar")
                .env_remove("TAR_OPTIONS")
                .arg("-xf")
                .arg(archive)
                .args([
                    "--strip-components=1",
                    "--no-same-owner",
                    "--no-same-permissions",
                    "-C",
                ])
                .arg(temporary.path()),
            &work.join("toolchain.log"),
        )?;
        compiler_path(temporary.path(), compiler)?;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(temporary.path().join(STAMP))?
            .write_all(checksum.as_bytes())?;
        fs::rename(temporary.path(), &directory)
            .with_context(|| format!("publishing Go toolchain {}", directory.display()))?;
    }
    compiler_path(&directory, compiler)
}

pub fn build(
    recipe: &Recipe,
    variables: &BTreeMap<String, String>,
    archive: Option<&Path>,
) -> Result<backend::BuildOutput> {
    check_host(recipe)?;
    ensure!(
        archive.is_some() == recipe.toolchain.is_some(),
        "Go build toolchain archive does not match the recipe"
    );
    let work = Path::new(variables.get("work").context("missing Go work directory")?);
    let mut variables = variables.clone();
    if let Some(archive) = archive {
        let compiler = toolchain(
            archive,
            work,
            recipe.go.as_ref().unwrap().compiler.as_deref().unwrap(),
        )?;
        variables.insert(
            "cc".into(),
            compiler
                .to_str()
                .context("Go compiler path must be UTF-8")?
                .into(),
        );
    }
    process::logged(&mut command(recipe, &variables)?, &work.join("go.log"))?;
    Ok(backend::BuildOutput {
        primary: backend::output_path(recipe.build_output.as_deref().unwrap(), &variables)?,
        additional: recipe
            .artifacts
            .iter()
            .map(|(path, name)| Ok((name.clone(), backend::output_path(path, &variables)?)))
            .collect::<Result<_>>()?,
    })
}

fn command(recipe: &Recipe, variables: &BTreeMap<String, String>) -> Result<Command> {
    validate(recipe)?;
    let settings = recipe.go.as_ref().context("missing go settings")?;
    let cwd = backend::output_path(settings.cwd.as_deref().unwrap_or("${source}"), variables)?;
    let mut command = Command::new(program());
    isolate_go(&mut command);
    command.current_dir(cwd);
    for arg in &recipe.args {
        command.arg(backend::expand(arg, variables)?);
    }
    for (key, value) in &settings.env {
        let key = backend::expand(key, variables)?;
        environment_key(&key)?;
        command.env(key, backend::expand(value, variables)?);
    }
    Ok(command)
}

pub fn program() -> PathBuf {
    if cfg!(target_os = "linux")
        && let Some(home) = std::env::var_os("HOME")
    {
        let installed = Path::new(&home).join(".local/go/bin/go");
        if installed.is_file() {
            return installed;
        }
    }
    PathBuf::from("go")
}

#[derive(Deserialize)]
struct Release {
    version: String,
    files: Vec<Distribution>,
}

#[derive(Deserialize)]
struct Distribution {
    filename: String,
    os: String,
    arch: String,
    sha256: String,
    kind: String,
}

fn install_go(version: &str) -> Result<PathBuf> {
    ensure!(
        cfg!(target_os = "linux"),
        "Go is not installed; install Go from https://go.dev/dl/ and add it to PATH"
    );
    let home = std::env::var_os("HOME").context("HOME is required to install Go")?;
    let destination = Path::new(&home).join(".local/go");
    ensure!(
        fs::symlink_metadata(&destination)
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
        "Go is missing from PATH, but {} already exists or is inaccessible; add its bin directory to PATH or resolve it manually",
        destination.display()
    );
    let result = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--retry",
            "3",
            "--connect-timeout",
            "30",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--",
            "https://go.dev/dl/?mode=json&include=all",
        ])
        .output()
        .context("reading official Go download metadata")?;
    ensure!(
        result.status.success(),
        "reading official Go download metadata: {}",
        String::from_utf8_lossy(&result.stderr).trim()
    );
    let releases: Vec<Release> = serde_json::from_slice(&result.stdout)?;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "x86" => "386",
        "aarch64" => "arm64",
        "arm" => "armv6l",
        arch => arch,
    };
    let distribution = releases
        .iter()
        .find(|release| release.version == version)
        .and_then(|release| {
            release.files.iter().find(|file| {
                file.os == "linux"
                    && file.arch == arch
                    && file.kind == "archive"
                    && file.filename.ends_with(".tar.gz")
            })
        })
        .with_context(|| format!("official {version} archive for linux/{arch} was not found"))?;
    ensure!(
        distribution
            .filename
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte)),
        "invalid Go archive filename in download metadata"
    );
    let download = tempfile::tempdir()?;
    let archive = source::fetch(
        &source::Source {
            url: format!("https://go.dev/dl/{}", distribution.filename),
            sha256: distribution.sha256.clone(),
            version: version.into(),
        },
        download.path(),
    )?;
    source::extract(&archive, &destination)?;
    println!("[ok] installed {version} in {}", destination.display());
    Ok(destination.join("bin/go"))
}

pub fn setup(toolchains: &BTreeSet<&str>) -> Result<()> {
    let Some(first) = toolchains.first() else {
        return Ok(());
    };
    let go = program();
    let mut probe = Command::new(&go);
    isolate_go(&mut probe);
    probe.env("GOTOOLCHAIN", "local").arg("version");
    let go = match probe.output() {
        Ok(result) => {
            ensure!(
                result.status.success(),
                "existing Go installation failed: {}",
                String::from_utf8_lossy(&result.stderr).trim()
            );
            go
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => install_go(first)?,
        Err(error) => return Err(error).context("checking Go installation"),
    };
    let directory = tempfile::tempdir()?;
    for version in toolchains {
        let mut command = Command::new(&go);
        isolate_go(&mut command);
        let result = command
            .env("GOTOOLCHAIN", version)
            .env("GOWORK", "off")
            .arg("version")
            .current_dir(directory.path())
            .output()
            .with_context(|| format!("setting up {version}"))?;
        let actual = String::from_utf8_lossy(&result.stdout);
        ensure!(
            result.status.success() && actual.split_whitespace().nth(2) == Some(version),
            "could not select {version}; use Go 1.21 or newer with toolchain downloads enabled: {}{}",
            actual,
            String::from_utf8_lossy(&result.stderr)
        );
        println!("[ok] {}", actual.trim());
    }
    Ok(())
}

fn isolate_go(command: &mut Command) {
    // Build settings come from the recipe. Network proxy and authentication
    // settings remain available for fetching pinned module dependencies.
    for variable in [
        "GO111MODULE",
        "GOOS",
        "GOARCH",
        "CGO_ENABLED",
        "GOFLAGS",
        "GO_EXTLINK_ENABLED",
        "GOTOOLCHAIN",
        "GOWORK",
        "GOROOT",
        "GOPATH",
        "GOBIN",
        "GOCACHE",
        "GOCACHEPROG",
        "GOMODCACHE",
        "GOENV",
        "GOEXPERIMENT",
        "GODEBUG",
        "GO386",
        "GOAMD64",
        "GOARM",
        "GOARM64",
        "GOMIPS",
        "GOMIPS64",
        "GOPPC64",
        "GORISCV64",
        "GOWASM",
        "GOFIPS140",
        "GCCGO",
        "GCCGOTOOLDIR",
        "CC",
        "CXX",
        "FC",
        "AR",
        "PKG_CONFIG",
    ] {
        command.env_remove(variable);
    }
    for flags in ["CFLAGS", "CPPFLAGS", "CXXFLAGS", "FFLAGS", "LDFLAGS"] {
        for suffix in ["", "_ALLOW", "_DISALLOW"] {
            command.env_remove(format!("CGO_{flags}{suffix}"));
        }
    }
    // Disable settings persisted by `go env -w`; [go.env] may opt back in.
    command.env("GOENV", "off");
}
