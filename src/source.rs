//! Pinned HTTPS downloads and transactional extraction of single-root tar.gz sources.
//!
//! Source snapshots omit top-level `.git`, `.agents`, and `.claude` metadata.
//! Windows materializes archive links as copies; other hosts retain link semantics.
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
    #[cfg(windows)]
    let mut links = WindowsLinks::default();
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
        #[cfg(windows)]
        {
            validate_windows_archive_path(&path, false)?;
            ensure!(
                !relative.to_string_lossy().eq_ignore_ascii_case(STAMP),
                "source archive uses Forge's reserved stamp filename"
            );
            let kind = entry.header().entry_type();
            if kind.is_symlink() || kind.is_hard_link() {
                let target = entry
                    .link_name()?
                    .context("archive link has no target")?
                    .into_owned();
                validate_windows_archive_path(&target, kind.is_symlink())?;
                let target = if kind.is_hard_link() {
                    target
                        .strip_prefix(Path::new(top))
                        .context("archive hard link target is outside its source root")?
                        .to_path_buf()
                } else {
                    target
                };
                links.insert(relative, target, kind.is_hard_link())?;
                continue;
            }
            ensure!(
                kind.is_file() || kind.is_contiguous() || kind.is_dir() || kind.is_gnu_sparse(),
                "unsupported Windows source archive member: {}",
                path.display()
            );
        }
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
    #[cfg(windows)]
    links.materialize(&tree)?;
    fs::write(tree.join(STAMP), checksum)?;
    fs::rename(&tree, destination)
        .with_context(|| format!("publishing source directory {}", destination.display()))?;
    Ok(())
}

#[cfg(windows)]
fn validate_windows_archive_path(path: &Path, parents: bool) -> Result<()> {
    ensure!(!path.as_os_str().is_empty(), "empty archive path");
    for component in path.components() {
        match component {
            Component::CurDir | Component::ParentDir if parents => {}
            Component::Normal(name) => {
                let name = name.to_str().context("archive path is not Unicode")?;
                let stem = name.split('.').next().unwrap_or_default().to_uppercase();
                let device = matches!(
                    stem.as_str(),
                    "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
                ) || ["COM", "LPT"].iter().any(|prefix| {
                    stem.strip_prefix(prefix).is_some_and(|suffix| {
                        matches!(
                            suffix,
                            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                        )
                    })
                });
                ensure!(
                    !device
                        && !name.ends_with(['.', ' '])
                        && !name
                            .chars()
                            .any(|c| c.is_control() || "<>:\"|?*".contains(c)),
                    "archive path cannot be represented safely on Windows: {}",
                    path.display()
                );
            }
            _ => anyhow::bail!("archive path is not relative: {}", path.display()),
        }
    }
    Ok(())
}

#[cfg(windows)]
#[derive(Clone)]
struct WindowsLink {
    path: PathBuf,
    target: PathBuf,
    hard: bool,
}

#[cfg(windows)]
#[derive(Default)]
struct WindowsLinks {
    entries: BTreeMap<PathBuf, WindowsLink>,
    active: std::collections::BTreeSet<PathBuf>,
    complete: std::collections::BTreeSet<PathBuf>,
}

// Archive paths have already been checked for Windows aliases such as trailing
// dots and ADS. Fold case as well when identifying links and their ancestors.
#[cfg(windows)]
fn windows_path_key(path: &Path) -> PathBuf {
    path.components()
        .map(|part| part.as_os_str().to_string_lossy().to_lowercase())
        .collect()
}

#[cfg(windows)]
impl WindowsLinks {
    fn insert(&mut self, path: &Path, target: PathBuf, hard: bool) -> Result<()> {
        ensure!(!path.as_os_str().is_empty(), "source root cannot be a link");
        ensure!(
            self.entries
                .insert(
                    windows_path_key(path),
                    WindowsLink {
                        path: path.to_path_buf(),
                        target,
                        hard,
                    },
                )
                .is_none(),
            "duplicate archive link: {}",
            path.display()
        );
        Ok(())
    }

    fn materialize(&mut self, tree: &Path) -> Result<()> {
        // No archive member may live beneath a link: that would make its
        // meaning depend on extraction order or overwrite a copied target.
        for (key, link) in &self.entries {
            ensure!(
                windows_tree_metadata(tree, &link.path)?.is_none(),
                "archive link conflicts with another member: {}",
                link.path.display()
            );
            for ancestor in key.ancestors().skip(1) {
                ensure!(
                    !self.entries.contains_key(ancestor),
                    "archive link is nested beneath another link: {}",
                    link.path.display()
                );
            }
        }
        for link in self.entries.values() {
            fs::create_dir_all(tree.join(link.path.parent().unwrap()))?;
        }
        for key in self.entries.keys().cloned().collect::<Vec<_>>() {
            self.materialize_link(tree, &key)?;
        }
        Ok(())
    }

    fn resolve(
        &self,
        tree: &Path,
        key: &Path,
        active: &mut std::collections::BTreeSet<PathBuf>,
    ) -> Result<PathBuf> {
        ensure!(
            active.insert(key.to_path_buf()),
            "archive link cycle at {}",
            key.display()
        );
        let link = &self.entries[key];
        let mut resolved = if link.hard {
            PathBuf::new()
        } else {
            link.path.parent().unwrap().to_path_buf()
        };
        for component in link.target.components() {
            ensure!(
                windows_tree_metadata(tree, &resolved)?.is_some_and(|m| m.is_dir()),
                "archive link traverses a missing or non-directory target: {}",
                link.path.display()
            );
            match component {
                Component::CurDir => {}
                Component::ParentDir => {
                    ensure!(
                        resolved.pop(),
                        "archive link escapes its source root: {}",
                        link.path.display()
                    );
                }
                Component::Normal(name) => {
                    resolved.push(name);
                    let target_key = windows_path_key(&resolved);
                    if self.entries.contains_key(&target_key) {
                        resolved = self.resolve(tree, &target_key, active)?;
                    }
                }
                _ => anyhow::bail!("archive link target is not relative"),
            }
        }
        active.remove(key);
        Ok(resolved)
    }

    fn materialize_link(&mut self, tree: &Path, key: &Path) -> Result<()> {
        if self.complete.contains(key) {
            return Ok(());
        }
        ensure!(
            self.active.insert(key.to_path_buf()),
            "archive directory link cycle at {}",
            key.display()
        );
        let link = self.entries[key].clone();
        let source = self.resolve(tree, key, &mut Default::default())?;
        let metadata = windows_tree_metadata(tree, &source)?.with_context(|| {
            format!(
                "archive link target does not exist: {} -> {}",
                link.path.display(),
                source.display()
            )
        })?;
        // Resolve existing Windows aliases (including short names) before
        // checking which deferred links belong to this directory.
        let source = fs::canonicalize(tree.join(&source))?
            .strip_prefix(fs::canonicalize(tree)?)
            .context("archive link target is outside its source root")?
            .to_path_buf();
        ensure!(
            !link.hard || metadata.is_file(),
            "archive hard link target is not a file: {}",
            link.path.display()
        );
        if metadata.is_dir() {
            // Finish links inside the original directory before copying it.
            // Encountering an active link rejects self-containing directories
            // and indirect directory cycles before an unbounded copy begins.
            let source_key = windows_path_key(&source);
            let children = self
                .entries
                .keys()
                .filter(|path| path.starts_with(&source_key))
                .cloned()
                .collect::<Vec<_>>();
            for child in children {
                self.materialize_link(tree, &child)?;
            }
        }
        copy_windows_archive_node(tree, &source, &link.path)?;
        self.active.remove(key);
        self.complete.insert(key.to_path_buf());
        Ok(())
    }
}

/// Check every ancestor without following Windows symlinks or junctions.
#[cfg(windows)]
fn windows_tree_metadata(tree: &Path, relative: &Path) -> Result<Option<fs::Metadata>> {
    use std::os::windows::fs::MetadataExt;
    let mut path = tree.to_path_buf();
    let mut metadata = fs::symlink_metadata(&path)?;
    let mut components = relative.components();
    loop {
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "archive path is a Windows reparse point: {}",
            path.display()
        );
        let Some(component) = components.next() else {
            return Ok(Some(metadata));
        };
        ensure!(
            metadata.is_dir(),
            "archive path parent is not a directory: {}",
            path.display()
        );
        ensure!(
            matches!(component, Component::Normal(_)),
            "archive path is not relative"
        );
        path.push(component.as_os_str());
        metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
    }
}

#[cfg(windows)]
fn copy_windows_archive_node(tree: &Path, source: &Path, destination: &Path) -> Result<()> {
    let metadata =
        windows_tree_metadata(tree, source)?.context("archive link target is missing")?;
    ensure!(
        windows_tree_metadata(tree, destination)?.is_none(),
        "archive link destination already exists: {}",
        destination.display()
    );
    if metadata.is_dir() {
        fs::create_dir(tree.join(destination))?;
        for entry in fs::read_dir(tree.join(source))? {
            let name = entry?.file_name();
            copy_windows_archive_node(tree, &source.join(&name), &destination.join(&name))?;
        }
    } else {
        ensure!(
            metadata.is_file(),
            "archive link target is not a file or directory: {}",
            source.display()
        );
        fs::copy(tree.join(source), tree.join(destination))?;
    }
    Ok(())
}
