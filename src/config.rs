use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
};

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
    #[serde(default)]
    pub packages: BTreeMap<String, PackageOverride>,
    #[serde(default)]
    pub rootfs_files: BTreeMap<String, RootfsFile>,
    pub qemu: Option<QemuConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageOverride {
    pub config: Option<String>,
    pub artifact: Option<String>,
    pub args: Option<Vec<String>>,
    pub patches: Option<Vec<String>>,
    pub artifacts: Option<BTreeMap<String, String>>,
    pub in_tree: Option<bool>,
    pub target: Option<String>,
    pub build_output: Option<String>,
    pub kbuild: Option<crate::backend::kbuild::KbuildConfig>,
    pub cargo: Option<crate::backend::cargo::CargoConfig>,
    pub go: Option<crate::backend::go::GoConfig>,
    pub prebuilt: Option<crate::backend::prebuilt::PrebuiltConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum RootfsFile {
    Package(String),
    Artifact {
        package: String,
        artifact: String,
        mode: Option<u32>,
    },
}

impl RootfsFile {
    pub fn package(&self) -> &str {
        match self {
            Self::Package(package) | Self::Artifact { package, .. } => package,
        }
    }

    pub fn artifact<'a>(&'a self, recipe: &'a Recipe) -> &'a str {
        match self {
            Self::Package(_) => &recipe.artifact,
            Self::Artifact { artifact, .. } => artifact,
        }
    }

    pub fn mode(&self) -> u32 {
        match self {
            Self::Artifact {
                mode: Some(mode), ..
            } => *mode,
            _ => 0o644,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuConfig {
    pub program: String,
    pub machine: String,
    pub cpu: String,
    pub memory: String,
    pub smp: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recipe {
    pub schema: u32,
    pub name: String,
    pub backend: String,
    pub source: String,
    pub artifact: String,
    pub build_output: Option<String>,
    pub kbuild: Option<crate::backend::kbuild::KbuildConfig>,
    pub cargo: Option<crate::backend::cargo::CargoConfig>,
    pub go: Option<crate::backend::go::GoConfig>,
    pub prebuilt: Option<crate::backend::prebuilt::PrebuiltConfig>,
    #[serde(default = "default_extract")]
    pub extract: bool,
    #[serde(default)]
    pub extra_sources: BTreeMap<String, String>,
    /// Additional repository inputs read by the build, included in cache keys.
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub host_tools: Vec<String>,
    #[serde(default)]
    pub rust_components: Vec<String>,
    #[serde(default)]
    pub cargo_tools: Vec<CargoTool>,
    #[serde(default)]
    pub in_tree: bool,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub artifacts: BTreeMap<String, String>,
    pub file: Option<String>,
    pub target: Option<String>,
    pub config: Option<String>,
    pub toolchain: Option<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default)]
    pub patches: Vec<String>,
}

fn default_extract() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct CargoTool {
    pub name: String,
    pub version: String,
    pub toolchain: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub schema: u32,
    pub toolchain: String,
    pub firmware: String,
    pub bootloader: Option<String>,
    pub kernel: String,
    pub rootfs: Rootfs,
    pub boot: BootConfig,
    #[serde(default)]
    pub rootfs_files: BTreeMap<String, RootfsFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Rootfs {
    Busybox {
        package: String,
        init: String,
    },
    Prebuilt {
        package: String,
        #[serde(default)]
        scripts: BTreeMap<String, String>,
        #[serde(default)]
        gzip_overlay: bool,
    },
    /// Export the root filesystem package for deployment outside Forge.
    External {
        package: String,
    },
}

impl Rootfs {
    pub fn package(&self) -> &str {
        match self {
            Self::Busybox { package, .. }
            | Self::Prebuilt { package, .. }
            | Self::External { package } => package,
        }
    }

    pub fn uses_initramfs(&self) -> bool {
        !matches!(self, Self::External { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BootConfig {
    pub append: String,
    pub test_append: String,
    pub script: Option<String>,
    pub cpu: Option<String>,
    pub memory: Option<String>,
    pub smp: Option<u32>,
    #[serde(default)]
    pub qemu_args: Vec<String>,
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

pub fn validate_filename(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)),
        "invalid artifact filename {name:?}"
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

pub fn recipes(root: &Path, preset: &Preset, board: &Board) -> Result<BTreeMap<String, Recipe>> {
    let mut pending = preset.packages.clone();
    let mut result = BTreeMap::new();
    while let Some(name) = pending.pop() {
        if result.contains_key(&name) {
            continue;
        }
        validate_name(&name)?;
        let mut recipe: Recipe =
            read(&input_path(root, &format!("packages/{name}/package.toml"))?)?;
        if let Some(package) = board.packages.get(&name) {
            if let Some(config) = &package.config {
                recipe.config = Some(config.clone());
            }
            if let Some(artifact) = &package.artifact {
                recipe.artifact.clone_from(artifact);
            }
            if let Some(args) = &package.args {
                recipe.args.clone_from(args);
            }
            if let Some(patches) = &package.patches {
                recipe.patches.clone_from(patches);
            }
            if let Some(artifacts) = &package.artifacts {
                recipe.artifacts.clone_from(artifacts);
            }
            if let Some(in_tree) = package.in_tree {
                recipe.in_tree = in_tree;
            }
            if let Some(target) = &package.target {
                recipe.target = Some(target.clone());
            }
            if let Some(output) = &package.build_output {
                recipe.build_output = Some(output.clone());
            }
            if let Some(kbuild) = &package.kbuild {
                recipe.kbuild = Some(kbuild.clone());
            }
            if let Some(cargo) = &package.cargo {
                recipe.cargo = Some(cargo.clone());
            }
            if let Some(go) = &package.go {
                recipe.go = Some(go.clone());
            }
            if let Some(prebuilt) = &package.prebuilt {
                recipe.prebuilt = Some(prebuilt.clone());
            }
        }
        ensure!(
            recipe.schema == 1 && recipe.name == name,
            "invalid recipe schema/name for {name}"
        );
        ensure!(
            crate::backend::BACKENDS.contains(&recipe.backend.as_str()),
            "unsupported backend {}",
            recipe.backend
        );
        ensure!(
            (recipe.backend == "kbuild") == recipe.kbuild.is_some(),
            "{name}: kbuild backend requires [kbuild] settings"
        );
        ensure!(
            (recipe.backend == "cargo") == recipe.cargo.is_some(),
            "{name}: [cargo] settings are required only for the cargo backend"
        );
        if recipe.backend == "cargo" {
            crate::backend::cargo::validate(&recipe)?;
        }
        ensure!(
            (recipe.backend == "go") == recipe.go.is_some(),
            "{name}: [go] settings are required only for the go backend"
        );
        if recipe.backend == "go" {
            crate::backend::go::validate(&recipe)?;
        }
        ensure!(
            recipe.backend == "prebuilt" || recipe.prebuilt.is_none(),
            "{name}: [prebuilt] settings are only supported by the prebuilt backend"
        );
        if recipe.backend == "prebuilt" {
            crate::backend::prebuilt::prepare(&mut recipe)?;
            crate::backend::prebuilt::validate(&recipe)?;
        }
        ensure!(
            recipe.backend == "prebuilt" || recipe.file.is_none(),
            "{name}: file is only supported by prebuilt packages"
        );
        let archive_builder = recipe.backend == "kbuild";
        ensure!(
            recipe.toolchain.is_none() || archive_builder || recipe.backend == "go",
            "{name}: a toolchain archive requires the kbuild or go backend"
        );
        ensure!(
            archive_builder || !recipe.in_tree,
            "{name}: in_tree is only supported by archive builders"
        );
        ensure!(
            !archive_builder || recipe.extra_sources.is_empty(),
            "{name}: extra_sources currently require a native builder"
        );
        if archive_builder {
            ensure!(
                recipe.extract,
                "{name}: archive builders require source extraction"
            );
            crate::backend::kbuild::validate(&recipe)?;
        }
        for alias in recipe.extra_sources.keys() {
            validate_name(alias)?;
        }
        for input in &recipe.inputs {
            ensure!(
                input_path(root, input)?.is_file(),
                "input must be a file: {input}"
            );
        }
        ensure!(
            (recipe.extract && (recipe.backend != "prebuilt" || recipe.file.is_some()))
                || recipe.patches.is_empty(),
            "{name}: patches require source extraction"
        );
        validate_filename(&recipe.artifact)?;
        if let Some(file) = &recipe.file {
            validate_filename(file)?;
        }
        for artifact in recipe.artifacts.values() {
            validate_filename(artifact)?;
        }
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
