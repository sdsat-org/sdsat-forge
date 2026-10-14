//! Select primary and additional artifacts from verified prebuilt sources.

use crate::{backend::BuildOutput, config::Recipe};
use anyhow::{Result, ensure};
use std::{collections::BTreeMap, path::Path};

pub fn validate(recipe: &Recipe) -> Result<()> {
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

pub fn build(
    recipe: &Recipe,
    source: &Path,
    variables: &BTreeMap<String, String>,
) -> Result<BuildOutput> {
    validate(recipe)?;
    let primary = recipe
        .file
        .as_ref()
        .map_or_else(|| source.to_path_buf(), |file| source.join(file));
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
