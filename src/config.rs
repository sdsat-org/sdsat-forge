use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

pub const DEFAULT_CONFIG: &str = "qemu_riscv64_asterinas";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub schema: u32,
    pub default_config: String,
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = path.canonicalize()?;
    #[cfg(windows)]
    {
        let text = path
            .to_str()
            .context("repository path must be valid UTF-8")?;
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        Ok(PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text)))
    }
    #[cfg(not(windows))]
    Ok(path)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    pub schema: u32,
    pub name: String,
    pub board: String,
    pub profile: String,
    pub packages: Vec<String>,
    pub lock: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Board {
    pub schema: u32,
    pub arch: String,
    pub firmware_target: String,
    pub kernel_target: String,
    pub firmware_config: String,
    pub qemu: QemuConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuConfig {
    pub program: String,
    pub machine: String,
    pub cpu: String,
    pub memory: String,
    pub smp: u32,
    pub append: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema: u32,
    pub name: String,
    pub backend: String,
    pub source: String,
    pub config: Option<String>,
    pub toolchain: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub patches: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub toolchain: String,
    pub init: String,
}

pub fn read<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    toml::from_str(
        &fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?,
    )
    .with_context(|| format!("parsing {}", path.display()))
}

pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'),
        "invalid name {name:?}; use letters, digits, '_' or '-'"
    );
    Ok(())
}

/// All repository references are relative and remain inside the repository.
pub fn input_path(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    ensure!(
        !relative.is_empty() && !relative.contains('\\') && !relative.contains(':'),
        "invalid repository path {relative:?}"
    );
    ensure!(
        path.components().all(|c| matches!(c, Component::Normal(_))),
        "repository path must be relative without '..': {relative}"
    );
    let resolved = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("resolving {relative}"))?;
    ensure!(
        resolved.starts_with(root.canonicalize()?),
        "repository path escapes root: {relative}"
    );
    absolute(&resolved)
}

pub fn load_preset(root: &Path, name: &str) -> Result<Preset> {
    validate_name(name)?;
    let preset: Preset = read(&root.join(format!("configs/{name}_defconfig.toml")))?;
    validate_preset(root, &preset)?;
    ensure!(
        preset.name == name,
        "preset name differs from its file name"
    );
    Ok(preset)
}

pub fn validate_preset(root: &Path, preset: &Preset) -> Result<()> {
    ensure!(
        preset.schema == 1,
        "unsupported preset schema {}",
        preset.schema
    );
    validate_name(&preset.name)?;
    validate_name(&preset.profile)?;
    input_path(root, &format!("boards/{}/board.toml", preset.board))?;
    input_path(root, &preset.lock)?;
    for name in &preset.packages {
        validate_name(name)?;
    }
    ensure!(!preset.packages.is_empty(), "preset has no packages");
    Ok(())
}

pub fn recipes(root: &Path, preset: &Preset) -> Result<BTreeMap<String, Recipe>> {
    let mut pending = preset.packages.clone();
    let mut result = BTreeMap::new();
    while let Some(name) = pending.pop() {
        if result.contains_key(&name) {
            continue;
        }
        validate_name(&name)?;
        let recipe: Recipe = read(&input_path(root, &format!("packages/{name}/package.toml"))?)?;
        ensure!(
            recipe.schema == 1 && recipe.name == name,
            "invalid recipe schema/name for {name}"
        );
        ensure!(
            ["rustsbi", "asterinas", "prebuilt", "busybox"].contains(&recipe.backend.as_str()),
            "unsupported backend {}",
            recipe.backend
        );
        pending.extend(recipe.dependencies.clone());
        result.insert(name, recipe);
    }
    Ok(result)
}

pub fn build_order(recipes: &BTreeMap<String, Recipe>, selected: &[String]) -> Result<Vec<String>> {
    fn visit(
        name: &str,
        recipes: &BTreeMap<String, Recipe>,
        active: &mut BTreeSet<String>,
        done: &mut BTreeSet<String>,
        order: &mut Vec<String>,
    ) -> Result<()> {
        if done.contains(name) {
            return Ok(());
        }
        ensure!(active.insert(name.to_owned()), "dependency cycle at {name}");
        let recipe = recipes
            .get(name)
            .with_context(|| format!("unknown package {name}"))?;
        for dependency in &recipe.dependencies {
            visit(dependency, recipes, active, done, order)?;
        }
        active.remove(name);
        done.insert(name.to_owned());
        order.push(name.to_owned());
        Ok(())
    }
    let mut order = Vec::new();
    let (mut active, mut done) = (BTreeSet::new(), BTreeSet::new());
    for name in selected {
        visit(name, recipes, &mut active, &mut done, &mut order)?;
    }
    if order.is_empty() {
        bail!("empty build plan");
    }
    Ok(order)
}
