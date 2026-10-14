pub mod cargo;
pub mod kbuild;
pub mod prebuilt;

use anyhow::{Context, Result, ensure};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const BACKENDS: &[&str] = &["cargo", "kbuild", "prebuilt"];

pub struct BuildOutput {
    pub primary: PathBuf,
    pub additional: BTreeMap<String, PathBuf>,
}

/// Expand configuration variables once, without shell evaluation.
pub fn expand(text: &str, variables: &BTreeMap<String, String>) -> Result<String> {
    let mut remaining = text;
    let mut output = String::new();
    while let Some(start) = remaining.find("${") {
        output.push_str(&remaining[..start]);
        let expression = &remaining[start + 2..];
        let end = expression
            .find('}')
            .context("unterminated configuration variable")?;
        let name = &expression[..end];
        output.push_str(
            variables
                .get(name)
                .with_context(|| format!("unknown configuration variable {name:?}"))?,
        );
        remaining = &expression[end + 1..];
    }
    output.push_str(remaining);
    Ok(output)
}

pub fn output_path(text: &str, variables: &BTreeMap<String, String>) -> Result<PathBuf> {
    let expanded = expand(text, variables)?;
    ensure!(!expanded.is_empty(), "empty artifact path");
    Ok(Path::new(
        variables
            .get("work")
            .context("missing build work directory")?,
    )
    .join(expanded))
}
