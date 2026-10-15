//! Select primary and additional artifacts from verified prebuilt sources.

use crate::{backend::BuildOutput, config::Recipe};
use anyhow::{Context, Result, ensure};
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PrebuiltConfig {
    pub member: String,
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    if let Some(prebuilt) = &recipe.prebuilt {
        ensure!(
            !recipe.extract && recipe.file.is_none(),
            "{}: prebuilt member requires extract = false and cannot be combined with file",
            recipe.name
        );
        ensure!(
            prebuilt.member.split('/').all(|part| {
                !part.is_empty()
                    && part != "."
                    && part != ".."
                    && !part.contains(['\\', ':'])
                    && !part.chars().any(char::is_control)
            }),
            "{}: prebuilt member must be a safe relative archive path: {:?}",
            recipe.name,
            prebuilt.member
        );
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

fn extract_member(source: &Path, member: &str, output: &Path) -> Result<()> {
    let mut archive = tar::Archive::new(MultiGzDecoder::new(File::open(source)?));
    // Scan all concatenated tar/gzip streams before publishing a unique match.
    archive.set_ignore_zeros(true);
    let mut selected = None;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path_bytes();
        let mut path = path.as_ref();
        while let Some(relative) = path.strip_prefix(b"./") {
            path = relative;
        }
        if path != member.as_bytes() {
            continue;
        }
        ensure!(
            selected.is_none(),
            "duplicate prebuilt archive member: {member}"
        );
        ensure!(
            entry.header().entry_type().is_file(),
            "prebuilt archive member must be a regular file: {member}"
        );
        let mut temporary = tempfile::NamedTempFile::new_in(
            output
                .parent()
                .context("prebuilt output needs a parent directory")?,
        )?;
        let size = entry.size();
        ensure!(
            io::copy(&mut entry, temporary.as_file_mut())? == size,
            "truncated prebuilt archive member: {member}"
        );
        selected = Some(temporary);
    }
    let temporary =
        selected.with_context(|| format!("prebuilt archive member not found: {member}"))?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(output)
        .with_context(|| format!("publishing prebuilt member {}", output.display()))?;
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
        extract_member(source, &prebuilt.member, &output)?;
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
