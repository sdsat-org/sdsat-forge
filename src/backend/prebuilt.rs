//! Select primary and additional artifacts from verified prebuilt sources.

use crate::{
    backend::BuildOutput,
    config::{self, Recipe},
};
use anyhow::{Context, Result, ensure};
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrebuiltConfig {
    pub member: String,
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
}

/// Include archive members in the same cache, manifest and export paths as other artifacts.
pub fn prepare(recipe: &mut Recipe) -> Result<()> {
    validate(recipe)?;
    let Some(prebuilt) = &recipe.prebuilt else {
        return Ok(());
    };
    let mut names: BTreeSet<_> = std::iter::once(&recipe.artifact)
        .chain(recipe.artifacts.values())
        .map(|name| name.to_ascii_lowercase())
        .collect();
    for name in prebuilt.artifacts.values() {
        ensure!(
            names.insert(name.to_ascii_lowercase()),
            "duplicate prebuilt output artifact: {name}"
        );
        ensure!(
            !recipe.artifacts.contains_key(name),
            "prebuilt output conflicts with artifact path: {name}"
        );
        recipe.artifacts.insert(name.clone(), name.clone());
    }
    Ok(())
}

fn safe_member(member: &str) -> bool {
    member.split('/').all(|part| {
        !part.is_empty()
            && part != "."
            && part != ".."
            && !part.contains(['\\', ':'])
            && !part.chars().any(char::is_control)
    })
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    if let Some(prebuilt) = &recipe.prebuilt {
        ensure!(
            !recipe.extract && recipe.file.is_none(),
            "{}: prebuilt member requires extract = false and cannot be combined with file",
            recipe.name
        );
        for member in std::iter::once(&prebuilt.member).chain(prebuilt.artifacts.keys()) {
            ensure!(
                safe_member(member),
                "{}: prebuilt member must be a safe relative archive path: {member:?}",
                recipe.name
            );
        }
        ensure!(
            !prebuilt.artifacts.contains_key(&prebuilt.member),
            "duplicate prebuilt member selection: {}",
            prebuilt.member
        );
        for name in prebuilt.artifacts.values() {
            config::validate_filename(name)?;
        }
    }
    ensure!(
        recipe.file.is_none() || recipe.extract,
        "{}: file requires source extraction",
        recipe.name
    );
    ensure!(
        recipe.build_output.is_none(),
        "{}: prebuilt packages use source/file instead of build_output",
        recipe.name
    );
    ensure!(
        recipe.args.is_empty(),
        "{}: prebuilt packages do not execute build arguments",
        recipe.name
    );
    Ok(())
}

fn normalized_member(mut path: &[u8]) -> &[u8] {
    while let Some(relative) = path.strip_prefix(b"./") {
        path = relative;
    }
    path
}

type Selections = BTreeMap<String, tempfile::NamedTempFile>;

fn select_member(
    reader: &mut impl Read,
    member: &str,
    size: u64,
    output: &Path,
    selected: &mut Selections,
) -> Result<()> {
    ensure!(
        !selected.contains_key(member),
        "duplicate prebuilt archive member: {member}"
    );
    let mut temporary = tempfile::NamedTempFile::new_in(
        output
            .parent()
            .context("prebuilt output needs a parent directory")?,
    )?;
    ensure!(
        io::copy(reader, temporary.as_file_mut())
            .with_context(|| format!("reading prebuilt archive member {member}"))?
            == size,
        "truncated prebuilt archive member: {member}"
    );
    temporary.as_file().sync_all()?;
    selected.insert(member.to_owned(), temporary);
    Ok(())
}

fn extract_tar(
    source: &Path,
    members: &BTreeMap<String, PathBuf>,
    selected: &mut Selections,
) -> Result<()> {
    let mut archive = tar::Archive::new(MultiGzDecoder::new(File::open(source)?));
    // Scan all concatenated tar/gzip streams before publishing a unique match.
    archive.set_ignore_zeros(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path_bytes().into_owned();
        let Some((member, output)) = members
            .iter()
            .find(|(member, _)| member.as_bytes() == normalized_member(&path))
        else {
            continue;
        };
        ensure!(
            entry.header().entry_type().is_file(),
            "prebuilt archive member must be a regular file: {member}"
        );
        let size = entry.size();
        select_member(&mut entry, member, size, output, selected)?;
    }
    Ok(())
}

// ZipArchive indexes central entries by name, collapsing exact duplicates. Count
// the original records too, so an ambiguous archive cannot select the last one.
fn zip_entry_count(source: &Path, start: u64) -> Result<usize> {
    let mut file = File::open(source)?;
    let length = file.metadata()?.len();
    file.seek(SeekFrom::Start(start))?;
    let mut count = 0;
    loop {
        let mut signature = [0; 4];
        file.read_exact(&mut signature)?;
        if signature != *b"PK\x01\x02" {
            ensure!(
                [*b"PK\x05\x06", *b"PK\x06\x06", *b"PK\x05\x05"].contains(&signature),
                "invalid ZIP central directory"
            );
            return Ok(count);
        }
        let mut header = [0; 42];
        file.read_exact(&mut header)?;
        let variable_size: u64 = [24, 26, 28]
            .into_iter()
            .map(|i| u64::from(u16::from_le_bytes([header[i], header[i + 1]])))
            .sum();
        let end = file
            .stream_position()?
            .checked_add(variable_size)
            .context("ZIP central directory offset overflow")?;
        ensure!(end <= length, "truncated ZIP central directory");
        file.seek(SeekFrom::Start(end))?;
        count += 1;
    }
}

fn extract_zip(
    source: &Path,
    members: &BTreeMap<String, PathBuf>,
    selected: &mut Selections,
) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(source)?)?;
    ensure!(
        zip_entry_count(source, archive.central_directory_start())? == archive.len(),
        "duplicate prebuilt ZIP archive member"
    );
    for index in 0..archive.len() {
        let entry = archive.by_index_raw(index)?;
        let Some((member, output)) = members
            .iter()
            .find(|(member, _)| member.as_bytes() == normalized_member(entry.name_raw()))
        else {
            continue;
        };
        ensure!(
            entry.is_file()
                && entry
                    .unix_mode()
                    .is_none_or(|mode| mode & 0o170000 == 0o100000),
            "prebuilt archive member must be a regular file: {member}"
        );
        let size = entry.size();
        drop(entry);
        // Read to EOF: zip's reader checks the uncompressed CRC, including for
        // empty members. Never publish a partial or corrupt extraction.
        let mut entry = archive.by_index(index)?;
        select_member(&mut entry, member, size, output, selected)?;
    }
    Ok(())
}

fn extract_members(source: &Path, members: &BTreeMap<String, PathBuf>) -> Result<()> {
    let mut magic = [0; 4];
    File::open(source)?.read_exact(&mut magic)?;
    let mut selected = Selections::new();
    if magic.starts_with(b"PK") {
        extract_zip(source, members, &mut selected)?;
    } else {
        ensure!(
            magic.starts_with(b"\x1f\x8b"),
            "prebuilt member source must be a ZIP or gzip-compressed tar archive"
        );
        extract_tar(source, members, &mut selected)?;
    }
    for member in members.keys() {
        ensure!(
            selected.contains_key(member),
            "prebuilt archive member not found: {member}"
        );
    }
    for (member, temporary) in selected {
        let output = &members[&member];
        temporary
            .persist(output)
            .with_context(|| format!("publishing prebuilt member {}", output.display()))?;
    }
    Ok(())
}

pub fn build(
    recipe: &Recipe,
    source: &Path,
    variables: &BTreeMap<String, String>,
) -> Result<BuildOutput> {
    validate(recipe)?;
    let primary: PathBuf = if let Some(prebuilt) = &recipe.prebuilt {
        let output = crate::backend::output_path(&recipe.artifact, variables)?;
        let mut members = BTreeMap::from([(prebuilt.member.clone(), output.clone())]);
        for (member, name) in &prebuilt.artifacts {
            members.insert(
                member.clone(),
                crate::backend::output_path(name, variables)?,
            );
        }
        extract_members(source, &members)?;
        output
    } else {
        recipe
            .file
            .as_ref()
            .map_or_else(|| source.to_path_buf(), |file| source.join(file))
    };
    let additional = recipe
        .artifacts
        .iter()
        .map(|(path, name)| {
            crate::backend::output_path(path, variables).map(|path| (name.clone(), path))
        })
        .collect::<Result<_>>()?;
    Ok(BuildOutput {
        primary,
        additional,
    })
}
