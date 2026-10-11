//! Pinned HTTPS downloads and transactional extraction of single-root tar.gz sources.
//!
//! Source snapshots omit top-level `.git`, `.agents`, and `.claude` metadata.
//! Other archive links retain their normal semantics and require host support.
use anyhow::{Context, Result, ensure};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{self, File},
    io,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceLock {
    pub schema: u32,
    pub sources: BTreeMap<String, LockedSource>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LockedSource {
    pub url: String,
    pub sha256: String,
    pub version: String,
}

pub fn load_lock(path: &Path) -> Result<SourceLock> {
    let lock: SourceLock = toml::from_str(&fs::read_to_string(path)?)
        .with_context(|| format!("reading source lock {}", path.display()))?;
    ensure!(
        lock.schema == 1,
        "unsupported source lock schema {}",
        lock.schema
    );
    ensure!(!lock.sources.is_empty(), "source lock must not be empty");
    for (name, source) in &lock.sources {
        validate(source).with_context(|| format!("invalid source {name}"))?;
    }
    Ok(lock)
}

fn validate(source: &LockedSource) -> Result<()> {
    ensure!(
        source.sha256.len() == 64 && source.sha256.bytes().all(|b| b.is_ascii_hexdigit()),
        "source SHA256 must contain exactly 64 hexadecimal digits"
    );
    ensure!(
        !source.version.trim().is_empty(),
        "source version must not be empty"
    );
    ensure!(
        source.url.starts_with("https://") && !source.url.chars().any(char::is_control),
        "source URL must use HTTPS without control characters"
    );
    Ok(())
}

/// Cached archives are revalidated; failed downloads never become cache entries.
pub fn fetch(source: &LockedSource, download_dir: &Path) -> Result<PathBuf> {
    validate(source)?;
    fs::create_dir_all(download_dir)?;
    let expected = source.sha256.to_ascii_lowercase();
    let cached = download_dir.join(format!("{expected}.tar"));
    if cached.try_exists()? {
        ensure!(
            fs::symlink_metadata(&cached)?.is_file(),
            "source cache is not a regular file"
        );
        if hash_file(&cached)? == expected {
            return Ok(cached);
        }
        eprintln!(
            "[forge] discarding corrupt cached source {}",
            cached.display()
        );
        fs::remove_file(&cached)?;
    }
    let temporary = tempfile::NamedTempFile::new_in(download_dir)?;
    crate::process::run(
        "curl",
        [
            OsStr::new("--fail"),
            OsStr::new("--location"),
            OsStr::new("--silent"),
            OsStr::new("--show-error"),
            OsStr::new("--retry"),
            OsStr::new("3"),
            OsStr::new("--connect-timeout"),
            OsStr::new("30"),
            OsStr::new("--proto"),
            OsStr::new("=https"),
            OsStr::new("--proto-redir"),
            OsStr::new("=https"),
            OsStr::new("--output"),
            temporary.path().as_os_str(),
            OsStr::new("--"),
            OsStr::new(&source.url),
        ],
        &std::env::current_dir()?,
    )?;
    let actual = hash_file(temporary.path())?;
    ensure!(
        actual == expected,
        "SHA256 mismatch for {}: expected {expected}, got {actual}",
        source.url
    );
    if let Err(error) = temporary.persist_noclobber(&cached) {
        // Another configuration may have downloaded this same content meanwhile.
        ensure!(
            hash_file(&cached).is_ok_and(|hash| hash == expected),
            "publishing verified source archive: {}",
            error.error
        );
    }
    Ok(cached)
}

fn hash_file(path: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    io::copy(&mut File::open(path)?, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Publish a complete source tree by rename. Existing trees need a matching stamp.
pub fn extract(archive: &Path, destination: &Path) -> Result<()> {
    const STAMP: &str = ".forge-source-sha256";
    let checksum = hash_file(archive)?;
    if destination.try_exists()? {
        ensure!(
            fs::symlink_metadata(destination)?.is_dir(),
            "source destination is not a directory"
        );
        ensure!(
            fs::read_to_string(destination.join(STAMP)).ok().as_deref() == Some(&checksum),
            "source directory {} is incomplete or changed; clean it before rebuilding",
            destination.display()
        );
        return Ok(());
    }
    let parent = destination
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = tempfile::tempdir_in(parent)?;
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(archive)?));
    let mut root = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.header().entry_type().is_pax_global_extensions() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let mut components = path.components();
        let Some(Component::Normal(top)) = components.next() else {
            anyhow::bail!("source archive must have one relative top-level directory");
        };
        let first = root.get_or_insert_with(|| top.to_owned());
        ensure!(
            first == top,
            "source archive has multiple top-level directories"
        );
        let relative = components.as_path();
        if matches!(components.next(), Some(Component::Normal(name))
            if [".git", ".agents", ".claude"].iter().any(|skip| name == OsStr::new(skip)))
        {
            continue;
        }
        ensure!(
            relative != Path::new(STAMP),
            "source archive uses Forge's reserved stamp filename"
        );
        ensure!(
            entry.unpack_in(temporary.path())?,
            "archive member escapes its extraction directory: {}",
            path.display()
        );
    }
    let tree = temporary
        .path()
        .join(root.context("source archive is empty")?);
    ensure!(
        fs::symlink_metadata(&tree)?.is_dir(),
        "source archive root is not a directory"
    );
    fs::write(tree.join(STAMP), checksum)?;
    fs::rename(&tree, destination)
        .with_context(|| format!("publishing source directory {}", destination.display()))?;
    Ok(())
}
