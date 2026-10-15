//! Delegate Forge commands to a persistent, isolated Linux SSH workspace.

use crate::{config, engine::Build, process};
use anyhow::{Context, Result, ensure};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
};

const SSH_HINT: &str = "Set an SSH Host alias or user@hostname, then retry:\n\
    PowerShell: $env:FORGE_BUILD_SSH = \"user@linux-host\"; just setup\n\
    sh: export FORGE_BUILD_SSH='user@linux-host'; just setup\n\
    Check key authentication with: ssh -o BatchMode=yes user@linux-host";

pub fn host() -> Result<Option<String>> {
    let Some(value) = std::env::var_os("FORGE_BUILD_SSH") else {
        return Ok(None);
    };
    let value = value
        .into_string()
        .map_err(|_| anyhow::anyhow!("FORGE_BUILD_SSH must be UTF-8\n{SSH_HINT}"))?;
    ensure!(
        !value.is_empty()
            && !value.starts_with('-')
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-@".contains(&b)),
        "FORGE_BUILD_SSH must be an SSH Host alias or user@hostname\n{SSH_HINT}"
    );
    Ok(Some(value))
}

fn ssh(host: &str) -> Command {
    let mut command = Command::new("ssh");
    command.args([
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=30",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ClearAllForwardings=yes",
        "-o",
        "SendEnv=-*",
        host,
    ]);
    command
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn checked_output(command: &mut Command) -> Result<String> {
    let output = command
        .output()
        .with_context(|| format!("starting SSH builder command\n{SSH_HINT}"))?;
    ensure!(
        output.status.success(),
        "SSH builder failed: {}\n{SSH_HINT}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    String::from_utf8(output.stdout).context("SSH builder returned non-UTF-8 output")
}

fn relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains(['\\', ':'])
            && !path.chars().any(char::is_control)
            && path
                .split('/')
                .all(|p| !p.is_empty() && p != "." && p != "..")
            && Path::new(path)
                .components()
                .all(|p| matches!(p, Component::Normal(_))),
        "unsafe remote archive path: {path:?}"
    );
    Ok(())
}

fn snapshot(build: &Build, destination: &Path) -> Result<String> {
    let files = Command::new("git")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(&build.root)
        .output()
        .context("listing repository snapshot inputs")?;
    ensure!(
        files.status.success(),
        "git ls-files failed: {}",
        String::from_utf8_lossy(&files.stderr)
    );
    let modes = Command::new("git")
        .args(["ls-files", "--stage", "-z"])
        .current_dir(&build.root)
        .output()
        .context("reading tracked file modes")?;
    ensure!(modes.status.success(), "git ls-files --stage failed");
    let executable = modes
        .stdout
        .split(|b| *b == 0)
        .filter_map(|record| {
            let tab = record.iter().position(|b| *b == b'\t')?;
            record
                .starts_with(b"100755 ")
                .then(|| record[tab + 1..].to_vec())
        })
        .collect::<BTreeSet<_>>();
    let mut entries = BTreeMap::new();
    for name in files
        .stdout
        .split(|b| *b == 0)
        .filter(|name| !name.is_empty())
    {
        let name = std::str::from_utf8(name).context("repository filenames must be UTF-8")?;
        relative(name)?;
        let top = name.split('/').next().unwrap();
        if [".git", ".forge", "output", "dl", "target", ".ssh", ".aws"].contains(&top)
            || top == ".env"
            || top.starts_with(".env.")
            || [
                ".netrc",
                ".npmrc",
                ".pypirc",
                ".cargo/credentials",
                ".cargo/credentials.toml",
            ]
            .contains(&name)
            || name == "forge.toml"
        {
            continue;
        }
        let path = build.root.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        ensure!(
            metadata.is_file(),
            "snapshot input must be a regular file: {name}"
        );
        ensure!(
            config::absolute(&path)?.starts_with(&build.root),
            "snapshot input escapes repository: {name}"
        );
        let mode = if executable.contains(name.as_bytes()) {
            0o755
        } else {
            0o644
        };
        entries.insert(name.to_owned(), (mode, fs::read(path)?));
    }
    entries.insert(
        "forge.toml".into(),
        (
            0o644,
            toml::to_string(&config::Project {
                schema: 1,
                default_config: build.preset.name.clone(),
            })?
            .into_bytes(),
        ),
    );
    entries.insert(
        format!("configs/{}_defconfig.toml", build.preset.name),
        (0o644, toml::to_string_pretty(&build.preset)?.into_bytes()),
    );
    let encoder = GzEncoder::new(File::create(destination)?, Compression::fast());
    let mut archive = tar::Builder::new(encoder);
    for (name, (mode, bytes)) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_size(bytes.len() as u64);
        header.set_cksum();
        archive.append_data(&mut header, name, bytes.as_slice())?;
    }
    archive.into_inner()?.finish()?.sync_all()?;
    crate::engine::digest_file(destination)
}

// The cache is owned by this user. Refuse symlinked components before creating
// or removing anything; only project inputs are replaced, never its caches.
const PREAMBLE: &str = r#"set -eu
umask 077
unset TAR_OPTIONS GZIP
test "$(uname -s)" = Linux || { echo 'Forge SSH builder requires Linux' >&2; exit 1; }
forge_home=$(CDPATH= cd -P "$HOME" && pwd)
export PATH="$forge_home/.cargo/bin:$forge_home/.local/go/bin:$PATH"
for tool in tar gzip sha256sum flock find mktemp; do
    command -v "$tool" >/dev/null || { echo "missing SSH builder tool: $tool" >&2; exit 1; }
done
safe_dir() {
    test ! -L "$1" || { echo "builder directory is a symlink: $1" >&2; exit 1; }
    if test ! -e "$1"; then mkdir -- "$1"; fi
    test -d "$1" && test "$(CDPATH= cd -P "$1" && pwd)" = "$1" || { echo "unsafe builder directory: $1" >&2; exit 1; }
}
safe_dir "$forge_home/.cache"
safe_dir "$forge_home/.cache/sdsat-forge"
base="$forge_home/.cache/sdsat-forge/@KEY@"
safe_dir "$base"
safe_dir "$base/archives"
safe_dir "$base/results"
safe_dir "$base/project"
test ! -L "$base/lock" || { echo 'builder lock is a symlink' >&2; exit 1; }
test ! -e "$base/lock" || test -f "$base/lock" || { echo 'builder lock is not a regular file' >&2; exit 1; }
exec 9>>"$base/lock"
flock 9
archive="$base/archives/@HASH@.tar.gz"
test ! -L "$archive" || { echo 'snapshot archive is a symlink' >&2; exit 1; }
test ! -e "$archive" || test -f "$archive" || { echo 'snapshot archive is not a regular file' >&2; exit 1; }
"#;

fn preamble(key: &str, hash: &str) -> String {
    PREAMBLE.replace("@KEY@", key).replace("@HASH@", hash)
}

fn run_script(host: &str, script: &str, directory: &Path, log: &Path) -> Result<()> {
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.write_all(script.as_bytes())?;
    file.flush()?;
    process::logged(ssh(host).arg("sh -s").stdin(File::open(file.path())?), log)
        .with_context(|| format!("running SSH builder command\n{SSH_HINT}"))
}

fn unpack(archive: &Path, directory: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut files = BTreeSet::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let name = entry.path_bytes();
        let name = std::str::from_utf8(&name).context("remote artifact path is not UTF-8")?;
        let name = name.trim_end_matches('/').to_owned();
        relative(&name)?;
        ensure!(
            [
                "images",
                "state",
                "manifests",
                ".config.toml",
                "resolved.lock.toml"
            ]
            .contains(&name.split('/').next().unwrap()),
            "unexpected remote output: {name}"
        );
        for part in name.split('/') {
            let stem = part.split('.').next().unwrap().to_ascii_uppercase();
            ensure!(
                !part.ends_with(['.', ' '])
                    && !part.contains(['<', '>', '"', '|', '?', '*'])
                    && !["CON", "PRN", "AUX", "NUL", "CONIN$", "CONOUT$"].contains(&stem.as_str())
                    && !["COM", "LPT"]
                        .iter()
                        .any(|p| stem.strip_prefix(p).is_some_and(|n| matches!(
                            n,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                        ))),
                "remote output cannot be represented safely: {name}"
            );
        }
        let kind = entry.header().entry_type();
        ensure!(
            kind.is_dir() || kind.is_file(),
            "remote outputs cannot contain links or devices: {name}"
        );
        let path = directory.join(&name);
        if kind.is_dir() {
            fs::create_dir_all(path)?;
        } else {
            ensure!(
                files.insert(name.to_lowercase()),
                "duplicate remote output: {name}"
            );
            fs::create_dir_all(path.parent().unwrap())?;
            let mut output = File::options().write(true).create_new(true).open(path)?;
            ensure!(
                io::copy(&mut entry, &mut output)? == entry.size(),
                "truncated remote output: {name}"
            );
        }
    }
    // Tar stops at its zero blocks before the gzip trailer. Read to EOF so a
    // truncated or corrupt compressed stream cannot be accepted as complete.
    io::copy(&mut archive.into_inner(), &mut io::sink())
        .context("validating remote archive gzip trailer")?;
    Ok(())
}

pub fn run(build: &Build, action: &str, package: Option<&str>) -> Result<PathBuf> {
    ensure!(
        ["build", "rebuild", "setup", "doctor"].contains(&action),
        "unsupported remote action: {action}"
    );
    if let Some(package) = package {
        config::validate_name(package)?;
    }
    let host = host()?.with_context(|| format!("FORGE_BUILD_SSH is not configured\n{SSH_HINT}"))?;
    fs::create_dir_all(&build.output)?;
    ensure!(
        config::absolute(&build.output)?.starts_with(&build.root),
        "remote output directory escapes repository"
    );
    let temporary = tempfile::Builder::new()
        .prefix("remote-transfer-")
        .tempdir_in(&build.output)?;
    let archive = temporary.path().join("snapshot.tar.gz");
    let hash = snapshot(build, &archive)?;
    let key = format!(
        "{:x}",
        Sha256::digest(build.root.to_string_lossy().as_bytes())
    );
    let prefix = preamble(&key[..16], &hash);
    let probe = format!(
        "{prefix}\nprintf '%s\\n' \"$base\"\nif test -f \"$archive\" && test \"$(sha256sum \"$archive\" | cut -d ' ' -f 1)\" = {hash}; then echo cached; else echo upload; fi\n"
    );
    let response = checked_output(ssh(&host).arg(format!("sh -c {}", quote(&probe))))?;
    let mut lines = response.lines();
    let base = lines.next().context("SSH builder omitted cache path")?;
    ensure!(
        base.starts_with('/')
            && base.ends_with(&format!("/.cache/sdsat-forge/{}", &key[..16]))
            && !base.chars().any(char::is_control),
        "invalid SSH builder cache path"
    );
    let status = lines
        .next()
        .context("SSH builder omitted snapshot status")?;
    ensure!(
        lines.next().is_none() && ["cached", "upload"].contains(&status),
        "invalid SSH builder response"
    );
    if status == "upload" {
        eprintln!(
            "[forge] uploading source snapshot {} to {host}",
            &hash[..16]
        );
        let upload = format!(
            "{prefix}\nupload=$(mktemp \"$base/archives/.upload.XXXXXXXX\")\ntrap 'rm -f -- \"$upload\"' EXIT\ncat > \"$upload\"\nprintf '%s  %s\\n' {hash} \"$upload\" | sha256sum -c - >&2\nmv -fT -- \"$upload\" \"$archive\"\n"
        );
        let output = ssh(&host)
            .arg(format!("sh -c {}", quote(&upload)))
            .stdin(File::open(&archive)?)
            .output()
            .with_context(|| format!("uploading Forge snapshot over SSH\n{SSH_HINT}"))?;
        ensure!(
            output.status.success(),
            "snapshot upload failed: {}\n{SSH_HINT}",
            String::from_utf8_lossy(&output.stderr)
        );
    } else {
        eprintln!("[forge] reusing source snapshot {} on {host}", &hash[..16]);
    }
    let mut script = prefix;
    script.push_str(&format!(
        "\nprintf '%s  %s\\n' {hash} \"$archive\" | sha256sum -c -\n"
    ));
    script.push_str(
        r#"project="$base/project"
for part in output dl target .forge; do safe_dir "$project/$part"; done
test ! -L "$base/snapshot" || { echo 'snapshot marker is a symlink' >&2; exit 1; }
"#,
    );
    script.push_str(&format!(
        "if test ! -f \"$base/snapshot\" || test \"$(cat \"$base/snapshot\")\" != {hash}; then\n"
    ));
    script.push_str(r#"    find "$project" -mindepth 1 -maxdepth 1 ! -name output ! -name dl ! -name target ! -name .forge -exec rm -rf -- {} +
    tar -xzf "$archive" -C "$project" --no-same-owner --no-same-permissions
    if test -d "$project/src"; then find "$project/src" -type f -exec touch -- {} +; fi
    find "$project" -maxdepth 1 -type f -name 'Cargo*' -exec touch -- {} +
    marker=$(mktemp "$base/.snapshot.XXXXXXXX")
"#);
    script.push_str(&format!(
        "    printf '%s' {hash} > \"$marker\"\n    mv -fT -- \"$marker\" \"$base/snapshot\"\nfi\n"
    ));
    let name = &build.preset.name;
    config::validate_name(name)?;
    script.push_str(&format!("safe_dir \"$project/output/{name}\"\nactive=$(mktemp \"$project/output/{name}/.config.XXXXXXXX\")\ncp -- \"$project/configs/{name}_defconfig.toml\" \"$active\"\nmv -fT -- \"$active\" \"$project/output/{name}/.config.toml\"\n"));
    script.push_str(r#"unset FORGE_BUILD_SSH RUSTFLAGS CARGO_ENCODED_RUSTFLAGS RUSTC RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER CARGO_BUILD_TARGET CARGO_TARGET_DIR
export FORGE_REMOTE_WORKER=1 RUSTUP_TOOLCHAIN=stable
"#);
    if action == "setup" {
        script.push_str(r#"if ! command -v rustup >/dev/null || ! command -v cargo >/dev/null; then
    installer=$(mktemp "$base/rustup.XXXXXXXX")
    curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error https://sh.rustup.rs -o "$installer"
    sh "$installer" -y --no-modify-path --profile minimal --default-toolchain stable
    rm -f -- "$installer"
fi
rustup toolchain install stable --profile minimal
"#);
    }
    script.push_str(r#"command -v rustup >/dev/null && command -v cargo >/dev/null && rustup run stable cargo --version >/dev/null 2>&1 || {
    echo 'Rust stable is missing on the SSH builder; run just setup with FORGE_BUILD_SSH set.' >&2
    exit 1
}
"#);
    for name in ["CARGO_BUILD_JOBS", "GOMAXPROCS"] {
        if let Some(value) = std::env::var_os(name) {
            let value = value
                .to_str()
                .context("parallelism setting must be UTF-8")?;
            ensure!(
                !value.is_empty()
                    && value.bytes().all(|b| b.is_ascii_digit())
                    && value.parse::<usize>().is_ok_and(|v| v > 0),
                "{name} must be a positive integer"
            );
            script.push_str(&format!("export {name}={value}\n"));
        }
    }
    script.push_str(&format!("cd \"$project\"\nCARGO_TARGET_DIR=\"$project/target\" cargo +stable run --locked --bin forge -- --config {} {}", quote(name), quote(action)));
    if let Some(package) = package {
        script.push(' ');
        script.push_str(&quote(package));
    }
    script.push('\n');
    let export = format!(
        "{base}/results/{}.tar.gz",
        temporary.path().file_name().unwrap().to_string_lossy()
    );
    if ["build", "rebuild"].contains(&action) {
        script.push_str(&format!("cd \"$project/output/{name}\"\nset --"));
        for path in build.remote_outputs(package)? {
            relative(&path)?;
            script.push(' ');
            script.push_str(&quote(&path));
        }
        script.push_str(
            r#"
for item do
    test -f "$item" && test ! -L "$item" || { echo "invalid remote output: $item" >&2; exit 1; }
    parent=$item
    while case "$parent" in */*) true;; *) false;; esac; do
        parent=${parent%/*}
        test ! -L "$parent" || { echo "remote output ancestor is a symlink: $parent" >&2; exit 1; }
    done
done
export_tmp=$(mktemp "$base/results/.export.XXXXXXXX")
trap 'rm -f -- "$export_tmp"' EXIT
tar -czf "$export_tmp" -- "$@"
"#,
        );
        script.push_str(&format!("mv -fT -- \"$export_tmp\" {}\n", quote(&export)));
    }
    let logs = build.output.join("logs");
    fs::create_dir_all(&logs)?;
    ensure!(
        config::absolute(&logs)?.starts_with(&build.root),
        "remote log directory escapes repository"
    );
    run_script(&host, &script, temporary.path(), &logs.join("remote.log"))?;
    if !["build", "rebuild"].contains(&action) {
        return Ok(build.output.clone());
    }
    let download = temporary.path().join("result.tar.gz");
    let result = ssh(&host)
        .arg(format!("cat -- {}", quote(&export)))
        .stdout(File::create(&download)?)
        .stderr(Stdio::piped())
        .output()
        .with_context(|| format!("downloading SSH build results\n{SSH_HINT}"))?;
    ensure!(
        result.status.success(),
        "remote result download failed: {}\n{SSH_HINT}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result = tempfile::Builder::new()
        .prefix("remote-result-")
        .tempdir_in(&build.output)?;
    unpack(&download, result.path())?;
    let cleanup = ssh(&host)
        .arg(format!("rm -f -- {}", quote(&export)))
        .status();
    if !cleanup.is_ok_and(|status| status.success()) {
        eprintln!("[forge] could not remove remote transfer archive {export}");
    }
    Ok(result.keep())
}
