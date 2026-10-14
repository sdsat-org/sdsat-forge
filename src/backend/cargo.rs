//! Configured Cargo commands without package-name dispatch.

use crate::{backend, config::Recipe, process};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path, process::Command};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CargoConfig {
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

fn environment_key(key: &str) -> Result<()> {
    ensure!(
        !key.is_empty() && !key.contains(['=', '\0']),
        "invalid Cargo environment key {key:?}"
    );
    Ok(())
}

pub fn validate(recipe: &Recipe) -> Result<()> {
    let settings = recipe.cargo.as_ref().context("missing cargo settings")?;
    ensure!(!recipe.args.is_empty(), "Cargo package requires args");
    ensure!(
        recipe
            .build_output
            .as_ref()
            .is_some_and(|path| !path.is_empty()),
        "Cargo package requires build_output"
    );
    if let Some(cwd) = &settings.cwd {
        ensure!(!cwd.is_empty(), "Cargo working directory cannot be empty");
    }
    for key in settings.env.keys() {
        environment_key(key)?;
    }
    Ok(())
}

pub fn build(
    recipe: &Recipe,
    variables: &BTreeMap<String, String>,
) -> Result<backend::BuildOutput> {
    validate(recipe)?;
    let work = Path::new(
        variables
            .get("work")
            .context("missing Cargo work directory")?,
    );
    process::logged(&mut command(recipe, variables)?, &work.join("cargo.log"))?;
    Ok(backend::BuildOutput {
        primary: backend::output_path(recipe.build_output.as_deref().unwrap(), variables)?,
        additional: recipe
            .artifacts
            .iter()
            .map(|(path, name)| Ok((name.clone(), backend::output_path(path, variables)?)))
            .collect::<Result<_>>()?,
    })
}

fn command(recipe: &Recipe, variables: &BTreeMap<String, String>) -> Result<Command> {
    validate(recipe)?;
    let settings = recipe.cargo.as_ref().context("missing cargo settings")?;
    let toolchain = backend::expand("${toolchain}", variables)?;
    ensure!(!toolchain.is_empty(), "Cargo toolchain cannot be empty");
    let cwd = backend::output_path(settings.cwd.as_deref().unwrap_or("${source}"), variables)?;
    let mut command = Command::new("cargo");
    process::isolate_rust(&mut command);
    command.arg(format!("+{toolchain}")).current_dir(cwd);
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
