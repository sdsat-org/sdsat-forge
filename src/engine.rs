use crate::{
    config::{self, Board, Preset, Profile, Recipe, Rootfs},
    image::{self, QemuOptions},
    source::{self, SourceLock},
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

pub struct Build {
    pub root: PathBuf,
    pub output: PathBuf,
    pub preset: Preset,
    pub board: Board,
    pub profile: Profile,
    recipes: BTreeMap<String, Recipe>,
    sources: SourceLock,
}

#[derive(Serialize, Deserialize)]
struct State {
    fingerprint: String,
    artifacts: BTreeMap<String, String>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    inputs: String,
    artifacts: BTreeMap<String, String>,
}

/// An OS lock releases automatically even if the builder crashes.
pub struct WorkspaceGuard {
    _file: File,
}
impl WorkspaceGuard {
    pub fn acquire(root: &Path, name: &str) -> Result<Self> {
        config::validate_name(name)?;
        let dir = root.join(".forge/locks");
        safe_directory(root, &dir)?;
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.join(name))?;
        file.try_lock()
            .context("configuration is in use by another Forge process")?;
        Ok(Self { _file: file })
    }
}

fn safe_directory(root: &Path, path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    ensure!(
        path.canonicalize()?.starts_with(root.canonicalize()?),
        "output directory escapes repository: {}",
        path.display()
    );
    Ok(())
}

pub fn digest_file(path: &Path) -> Result<String> {
    let mut hash = Sha256::new();
    io::copy(
        &mut File::open(path).with_context(|| format!("reading {}", path.display()))?,
        &mut hash,
    )?;
    Ok(format!("{:x}", hash.finalize()))
}

fn hash(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}

fn publish(path: &Path, bytes: &[u8]) -> Result<()> {
    use io::Write;
    let mut temporary =
        tempfile::NamedTempFile::new_in(path.parent().context("output needs a parent directory")?)?;
    temporary.write_all(bytes)?;
    temporary
        .persist(path)
        .with_context(|| format!("publishing {}", path.display()))?;
    Ok(())
}

impl Build {
    pub fn load(root: &Path, selected: Option<&str>) -> Result<Self> {
        let root = config::absolute(root)?;
        let project: config::Project = config::read(&root.join("forge.toml"))?;
        ensure!(project.schema == 1, "unsupported project schema");
        let name = selected
            .map(str::to_owned)
            .unwrap_or(project.default_config);
        config::validate_name(&name)?;
        let output = root.join("output").join(&name);
        let active = output.join(".config.toml");
        let preset: Preset = if active.exists() {
            config::read(&active)?
        } else {
            config::load_preset(&root, &name)?
        };
        config::validate_preset(&root, &preset)?;
        ensure!(
            preset.name == name,
            "config name differs from output directory"
        );
        let board: Board = config::read(&config::input_path(
            &root,
            &format!("boards/{}/board.toml", preset.board),
        )?)?;
        ensure!(
            board.schema == 1
                && board.arch == "riscv64"
                && board.qemu.as_ref().is_none_or(|q| q.smp > 0),
            "unsupported board schema/architecture/CPU count"
        );
        if let Some(sdcard) = &board.sdcard {
            config::validate_filename(&sdcard.dtb)?;
            ensure!(
                board.qemu.is_none(),
                "SD hardware and QEMU boot are separate board configurations"
            );
        }
        let profile: Profile = config::read(&config::input_path(
            &root,
            &format!("profiles/{}/profile.toml", preset.profile),
        )?)?;
        ensure!(profile.schema == 1, "unsupported profile schema");
        let sources = source::load_lock(&config::input_path(&root, &preset.lock)?)?;
        let recipes = config::recipes(&root, &preset, &board)?;
        let mut rootfs_paths = BTreeSet::new();
        for (path, file) in board.rootfs_files.iter().chain(&profile.rootfs_files) {
            ensure!(
                rootfs_paths.insert(path),
                "duplicate rootfs destination {path}"
            );
            ensure!(file.mode() <= 0o777, "invalid rootfs mode for {path}");
            for part in path.split('/') {
                config::validate_filename(part)?;
            }
            let package = file.package();
            let recipe = recipes.get(package).with_context(|| {
                format!("rootfs file {path} refers to missing package {package}")
            })?;
            let artifact = file.artifact(recipe);
            ensure!(
                artifact == recipe.artifact
                    || recipe.artifacts.values().any(|name| name == artifact),
                "rootfs file {path} refers to undeclared artifact {artifact} in {package}"
            );
        }
        let mut artifacts = BTreeSet::from(["initramfs.cpio"]);
        if board.sdcard.is_some() {
            artifacts.extend(image::spacemit_k1::ARTIFACTS.iter().copied());
        }
        for recipe in recipes.values() {
            ensure!(
                sources.sources.contains_key(&recipe.source),
                "unlocked source {}",
                recipe.source
            );
            for source in recipe.extra_sources.values() {
                ensure!(
                    sources.sources.contains_key(source),
                    "unlocked extra source {source}"
                );
            }
            for artifact in std::iter::once(&recipe.artifact).chain(recipe.artifacts.values()) {
                ensure!(
                    artifacts.insert(artifact),
                    "duplicate or reserved artifact {artifact}"
                );
            }
            for patch in &recipe.patches {
                config::input_path(&root, patch)?;
            }
            if let Some(path) = &recipe.config {
                config::input_path(&root, path)?;
            }
            if let Some(toolchain) = &recipe.toolchain {
                ensure!(
                    sources.sources.contains_key(toolchain),
                    "unlocked toolchain {toolchain}"
                );
            }
            if recipe.backend == "kbuild" {
                ensure!(
                    recipe.config.is_some() && recipe.toolchain.is_some(),
                    "{} needs a config and toolchain source",
                    recipe.name
                );
                ensure!(
                    recipe.patches.is_empty(),
                    "{} archive builds do not support patches",
                    recipe.name
                );
            }
        }
        for name in [
            profile.firmware.as_str(),
            profile.kernel.as_str(),
            profile.rootfs.package(),
        ]
        .into_iter()
        .chain(profile.bootloader.as_deref())
        {
            ensure!(
                recipes.contains_key(name),
                "profile refers to missing package {name}"
            );
        }
        ensure!(
            profile.boot.smp.is_none_or(|smp| smp > 0),
            "CPU count must be positive"
        );
        match &profile.rootfs {
            Rootfs::Busybox { init, .. } => {
                let directory = config::input_path(&root, init)?;
                for name in ["init", "boot-check.sh"] {
                    ensure!(
                        directory.join(name).is_file(),
                        "missing rootfs script {init}/{name}"
                    );
                }
            }
            Rootfs::Prebuilt { scripts, .. } => {
                for (name, path) in scripts {
                    for part in name.split('/') {
                        config::validate_filename(part)?;
                    }
                    config::input_path(&root, path)?;
                }
            }
        }
        let build = Self {
            root,
            output,
            preset,
            board,
            profile,
            recipes,
            sources,
        };
        build.order(None)?;
        Ok(build)
    }

    pub fn select(root: &Path, name: &str) -> Result<()> {
        let preset = config::load_preset(root, name)?;
        let _guard = WorkspaceGuard::acquire(root, name)?;
        let output = root.join("output").join(name);
        safe_directory(root, &output)?;
        publish(
            &output.join(".config.toml"),
            toml::to_string_pretty(&preset)?.as_bytes(),
        )?;
        publish(
            &root.join("forge.toml"),
            toml::to_string_pretty(&config::Project {
                schema: 1,
                default_config: name.into(),
            })?
            .as_bytes(),
        )?;
        println!("Selected {name}");
        Ok(())
    }

    fn order(&self, package: Option<&str>) -> Result<Vec<String>> {
        let selected = package
            .map(|p| vec![p.into()])
            .unwrap_or_else(|| self.preset.packages.clone());
        config::build_order(&self.recipes, &selected)
    }

    pub fn plan(&self) -> Result<()> {
        println!(
            "{} on {} -> {}",
            self.preset.name,
            self.preset.board,
            self.output.display()
        );
        for name in self.order(None)? {
            let recipe = &self.recipes[&name];
            println!(
                "  {name}: {} [{}]",
                self.sources.sources[&recipe.source].version, recipe.backend
            );
        }
        let stages = std::iter::once(self.profile.firmware.as_str())
            .chain(self.profile.bootloader.as_deref())
            .chain([self.profile.kernel.as_str(), self.profile.rootfs.package()])
            .collect::<Vec<_>>();
        println!("  Boot: {}", stages.join(" -> "));
        Ok(())
    }

    pub fn rust_targets(&self) -> Vec<&str> {
        self.recipes
            .values()
            .filter(|r| r.backend == "cargo")
            .filter_map(|r| r.target.as_deref())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn rust_components(&self) -> Vec<&str> {
        self.recipes
            .values()
            .flat_map(|r| r.rust_components.iter().map(String::as_str))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn host_tools(&self) -> Vec<&str> {
        self.recipes
            .values()
            .flat_map(|r| r.host_tools.iter().map(String::as_str))
            .chain(
                self.recipes
                    .values()
                    .any(|r| r.backend == "go")
                    .then_some("go"),
            )
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn cargo_tools(&self) -> Vec<&config::CargoTool> {
        self.recipes
            .values()
            .flat_map(|r| &r.cargo_tools)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn go_toolchains(&self) -> BTreeSet<&str> {
        self.recipes
            .values()
            .filter_map(|recipe| {
                recipe
                    .go
                    .as_ref()?
                    .env
                    .get("GOTOOLCHAIN")
                    .map(String::as_str)
            })
            .collect()
    }

    pub fn check_host(&self) -> Result<()> {
        for recipe in self.recipes.values() {
            if recipe.backend == "kbuild" {
                crate::backend::kbuild::check_host(recipe)?;
            }
            if recipe.backend == "go" {
                crate::backend::go::check_host(recipe)?;
            }
        }
        Ok(())
    }

    fn artifact(&self, name: &str) -> PathBuf {
        self.output
            .join("images")
            .join(&self.recipes[name].artifact)
    }

    fn system_artifacts(&self) -> Vec<&str> {
        self.recipes
            .values()
            .flat_map(|recipe| {
                std::iter::once(recipe.artifact.as_str())
                    .chain(recipe.artifacts.values().map(String::as_str))
            })
            .chain(["initramfs.cpio"])
            .chain(
                self.board
                    .sdcard
                    .iter()
                    .flat_map(|_| image::spacemit_k1::ARTIFACTS.iter().copied()),
            )
            .collect()
    }

    pub fn fetch_all(&self) -> Result<()> {
        for name in self.order(None)? {
            let recipe = &self.recipes[&name];
            for input in std::iter::once(&recipe.source)
                .chain(recipe.toolchain.iter())
                .chain(recipe.extra_sources.values())
            {
                source::fetch(&self.sources.sources[input], &self.root.join("dl"))?;
            }
        }
        Ok(())
    }

    fn patch_paths(&self, name: &str) -> Result<Vec<PathBuf>> {
        self.recipes[name]
            .patches
            .iter()
            .map(|p| config::input_path(&self.root, p))
            .collect()
    }

    fn source_dir(&self, name: &str) -> Result<PathBuf> {
        let mut key = self.sources.sources[&self.recipes[name].source]
            .sha256
            .clone();
        for patch in self.patch_paths(name)? {
            key.push_str(&digest_file(&patch)?);
        }
        Ok(self
            .output
            .join("build")
            .join(format!("{name}-{}", &hash(key)[..16]))
            .join("source"))
    }

    fn prepare_source(&self, name: &str) -> Result<PathBuf> {
        let source = self.source_dir(name)?;
        let archive = source::fetch(
            &self.sources.sources[&self.recipes[name].source],
            &self.root.join("dl"),
        )?;
        source::extract(&archive, &source)?;
        for (index, patch) in self.patch_paths(name)?.iter().enumerate() {
            let marker = source.join(format!(".forge-applied-{index}"));
            if marker.exists() {
                continue;
            }
            // Do not discover the enclosing Forge repository: git apply would
            // otherwise silently skip paths outside this nested source tree.
            for check in [true, false] {
                let mut cmd = Command::new("git");
                cmd.arg("apply").arg("--verbose");
                if check {
                    cmd.arg("--check");
                }
                cmd.arg(patch)
                    .current_dir(&source)
                    .env("GIT_CEILING_DIRECTORIES", source.parent().unwrap());
                ensure!(
                    cmd.status()
                        .context("applying source patch with git")?
                        .success(),
                    "could not apply {}",
                    patch.display()
                );
            }
            publish(&marker, digest_file(patch)?.as_bytes())?;
        }
        Ok(source)
    }

    fn fingerprint(&self, name: &str) -> Result<String> {
        let mut data = serde_json::to_vec(&(
            &self.recipes[name],
            &self.sources.sources[&self.recipes[name].source],
            &self.board,
            &self.profile.toolchain,
        ))?;
        data.extend_from_slice(include_bytes!("engine.rs"));
        let recipe = &self.recipes[name];
        data.extend_from_slice(include_bytes!("config.rs"));
        data.extend_from_slice(include_bytes!("process.rs"));
        data.extend_from_slice(include_bytes!("remote.rs"));
        data.extend_from_slice(include_bytes!("source.rs"));
        data.extend_from_slice(include_bytes!("backend/mod.rs"));
        data.extend_from_slice(include_bytes!("backend/cargo.rs"));
        if recipe.backend == "go" {
            data.extend_from_slice(include_bytes!("backend/go.rs"));
        }
        if recipe.backend == "kbuild" {
            data.extend_from_slice(include_bytes!("backend/kbuild.rs"));
        }
        if recipe.backend == "prebuilt" {
            data.extend_from_slice(include_bytes!("backend/prebuilt.rs"));
        }
        for source in recipe.extra_sources.values() {
            data.extend(serde_json::to_vec(&self.sources.sources[source])?);
        }
        if let Some(path) = &recipe.config {
            data.extend(fs::read(config::input_path(&self.root, path)?)?);
        }
        if let Some(toolchain) = &recipe.toolchain {
            data.extend(serde_json::to_vec(&self.sources.sources[toolchain])?);
        }
        data.extend_from_slice(std::env::consts::OS.as_bytes());
        data.extend_from_slice(std::env::consts::ARCH.as_bytes());
        for patch in self.patch_paths(name)? {
            data.extend(fs::read(patch)?);
        }
        for input in &recipe.inputs {
            data.extend(fs::read(config::input_path(&self.root, input)?)?);
        }
        for dependency in &self.recipes[name].dependencies {
            data.extend(fs::read(
                self.output.join(format!("state/{dependency}.json")),
            )?);
        }
        Ok(hash(data))
    }

    pub fn build(&self, package: Option<&str>, force: bool) -> Result<()> {
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        self.build_inner(package, force)
    }

    pub fn remote_outputs(&self, package: Option<&str>) -> Result<Vec<String>> {
        let mut paths = BTreeSet::from([".config.toml".to_owned()]);
        for name in self.order(package)? {
            let recipe = &self.recipes[&name];
            paths.insert(format!("state/{name}.json"));
            for artifact in std::iter::once(&recipe.artifact).chain(recipe.artifacts.values()) {
                paths.insert(format!("images/{artifact}"));
            }
        }
        if package.is_none() {
            paths.extend(
                self.system_artifacts()
                    .iter()
                    .map(|name| format!("images/{name}")),
            );
            paths.insert("manifests/build.json".into());
            paths.insert("resolved.lock.toml".into());
        }
        Ok(paths.into_iter().collect())
    }

    fn build_inner(&self, package: Option<&str>, force: bool) -> Result<()> {
        if crate::remote::host()?.is_some() {
            return self.build_remote(package, force);
        }
        let order = self.order(package)?;
        for dir in ["build", "images", "logs", "state", "manifests"] {
            safe_directory(&self.root, &self.output.join(dir))?;
        }
        publish(
            &self.output.join(".config.toml"),
            toml::to_string_pretty(&self.preset)?.as_bytes(),
        )?;
        let manifest = self.output.join("manifests/build.json");
        if manifest.exists() {
            fs::remove_file(&manifest)?;
        }
        for name in order {
            let recipe = &self.recipes[&name];
            let source = if recipe.backend == "kbuild"
                || !recipe.extract
                || (recipe.backend == "prebuilt" && recipe.file.is_none())
            {
                source::fetch(&self.sources.sources[&recipe.source], &self.root.join("dl"))?
            } else {
                self.prepare_source(&name)?
            };
            let fingerprint = self.fingerprint(&name)?;
            let artifact = self.artifact(&name);
            let state_path = self.output.join(format!("state/{name}.json"));
            let state = fs::read(&state_path)
                .ok()
                .and_then(|s| serde_json::from_slice::<State>(&s).ok());
            let cached = state.is_some_and(|s| {
                s.fingerprint == fingerprint
                    && std::iter::once(&recipe.artifact)
                        .chain(recipe.artifacts.values())
                        .all(|name| {
                            digest_file(&self.output.join("images").join(name))
                                .is_ok_and(|hash| s.artifacts.get(name) == Some(&hash))
                        })
            });
            if cached && !(force && package == Some(name.as_str())) {
                println!("[cached] {name}");
                continue;
            }
            let work = self.output.join(format!("build/{name}-work"));
            safe_directory(&self.root, &work)?;
            let variables = self.build_variables(recipe, &source, &work)?;
            let built = match recipe.backend.as_str() {
                "kbuild" => crate::backend::kbuild::build(
                    recipe,
                    &source,
                    &source::fetch(
                        &self.sources.sources[recipe.toolchain.as_ref().unwrap()],
                        &self.root.join("dl"),
                    )?,
                    &config::input_path(&self.root, recipe.config.as_deref().unwrap())?,
                    &work,
                )?,
                "cargo" => crate::backend::cargo::build(recipe, &variables)?,
                "go" => crate::backend::go::build(
                    recipe,
                    &variables,
                    recipe
                        .toolchain
                        .as_ref()
                        .map(|name| {
                            source::fetch(&self.sources.sources[name], &self.root.join("dl"))
                        })
                        .transpose()?
                        .as_deref(),
                )?,
                "prebuilt" => crate::backend::prebuilt::build(recipe, &source, &variables)?,
                _ => unreachable!("validated backend"),
            };
            if built.primary != artifact {
                publish(&artifact, &fs::read(&built.primary)?)?;
            }
            for (name, source) in built.additional {
                publish(
                    &self.output.join("images").join(&name),
                    &fs::read(&source)
                        .with_context(|| format!("reading artifact {}", source.display()))?,
                )?;
            }
            let artifacts = std::iter::once(&recipe.artifact)
                .chain(recipe.artifacts.values())
                .map(|name| {
                    Ok((
                        name.clone(),
                        digest_file(&self.output.join("images").join(name))?,
                    ))
                })
                .collect::<Result<_>>()?;
            let state = State {
                fingerprint,
                artifacts,
            };
            publish(&state_path, &serde_json::to_vec(&state)?)?;
        }
        if package.is_none() {
            self.initramfs()?;
            if let Some(sdcard) = &self.board.sdcard {
                image::spacemit_k1::create(
                    &self.output.join("images"),
                    &self.artifact(&self.profile.firmware),
                    &self.artifact(&self.profile.kernel),
                    &self.output.join("images/initramfs.cpio"),
                    &self.profile.boot.append,
                    &sdcard.dtb,
                )?;
            }
            let artifacts = self
                .system_artifacts()
                .iter()
                .map(|name| {
                    Ok((
                        (*name).into(),
                        digest_file(&self.output.join("images").join(name))?,
                    ))
                })
                .collect::<Result<_>>()?;
            publish(
                &self.output.join("resolved.lock.toml"),
                toml::to_string_pretty(&self.sources)?.as_bytes(),
            )?;
            publish(
                &manifest,
                &serde_json::to_vec_pretty(&Manifest {
                    inputs: self.inputs_digest()?,
                    artifacts,
                })?,
            )?;
        }
        Ok(())
    }

    fn build_remote(&self, package: Option<&str>, force: bool) -> Result<()> {
        let order = self.order(package)?;
        let action = if force { "rebuild" } else { "build" };
        let downloaded = crate::remote::run(self, action, package)?;
        let result = self.import_remote(&downloaded, &order, package.is_none());
        // The transport returns a freshly allocated directory under this output.
        let resolved = downloaded.canonicalize()?;
        ensure!(
            resolved.parent() == Some(self.output.canonicalize()?.as_path())
                && resolved
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("remote-result-")),
            "refusing to remove unexpected remote result directory"
        );
        fs::remove_dir_all(&resolved)?;
        result
    }

    fn import_remote(&self, downloaded: &Path, order: &[String], complete: bool) -> Result<()> {
        let mut expected = BTreeMap::new();
        let mut states = Vec::new();
        for name in order {
            let state: State =
                serde_json::from_slice(&fs::read(downloaded.join(format!("state/{name}.json")))?)?;
            let recipe = &self.recipes[name];
            for artifact in std::iter::once(&recipe.artifact).chain(recipe.artifacts.values()) {
                let checksum = state
                    .artifacts
                    .get(artifact)
                    .context("remote state lacks artifact checksum")?;
                expected.insert(artifact.clone(), checksum.clone());
            }
            states.push((name, state));
        }
        if complete {
            let manifest: Manifest =
                serde_json::from_slice(&fs::read(downloaded.join("manifests/build.json"))?)?;
            ensure!(
                manifest.inputs == self.inputs_digest()?,
                "remote configuration differs from local inputs"
            );
            for name in self.system_artifacts() {
                let checksum = manifest
                    .artifacts
                    .get(name)
                    .context("remote manifest lacks artifact checksum")?;
                if let Some(previous) = expected.insert(name.into(), checksum.clone()) {
                    ensure!(
                        previous == *checksum,
                        "conflicting remote checksum for {name}"
                    );
                }
            }
        }
        // Verify every result before publishing any local artifact.
        for (name, checksum) in &expected {
            ensure!(
                digest_file(&downloaded.join("images").join(name))? == *checksum,
                "remote artifact checksum mismatch: {name}"
            );
        }
        for dir in ["images", "state", "manifests"] {
            safe_directory(&self.root, &self.output.join(dir))?;
        }
        let manifest = self.output.join("manifests/build.json");
        if manifest.exists() {
            fs::remove_file(&manifest)?;
        }
        for name in expected.keys() {
            publish(
                &self.output.join("images").join(name),
                &fs::read(downloaded.join("images").join(name))?,
            )?;
        }
        for (name, state) in states {
            // Keep the Linux fingerprint: switching to a local builder must not
            // accidentally reuse a package compiled with different host tools.
            publish(
                &self.output.join(format!("state/{name}.json")),
                &serde_json::to_vec(&state)?,
            )?;
        }
        publish(
            &self.output.join(".config.toml"),
            toml::to_string_pretty(&self.preset)?.as_bytes(),
        )?;
        if complete {
            publish(
                &self.output.join("resolved.lock.toml"),
                toml::to_string_pretty(&self.sources)?.as_bytes(),
            )?;
            publish(
                &manifest,
                &serde_json::to_vec_pretty(&Manifest {
                    inputs: self.inputs_digest()?,
                    artifacts: expected,
                })?,
            )?;
        }
        Ok(())
    }

    fn build_variables(
        &self,
        recipe: &Recipe,
        source: &Path,
        work: &Path,
    ) -> Result<BTreeMap<String, String>> {
        let path_text = |path: &Path| -> Result<String> {
            Ok(path
                .to_str()
                .context("build paths must be UTF-8")?
                .replace('\\', "/"))
        };
        let mut variables = BTreeMap::from([
            ("root".into(), path_text(&self.root)?),
            ("source".into(), path_text(source)?),
            ("work".into(), path_text(work)?),
            ("images".into(), path_text(&self.output.join("images"))?),
            ("toolchain".into(), self.profile.toolchain.clone()),
        ]);
        if let Some(target) = &recipe.target {
            variables.insert("target".into(), target.clone());
        }
        if let Some(config) = &recipe.config {
            variables.insert(
                "config".into(),
                path_text(&config::input_path(&self.root, config)?)?,
            );
        }
        for dependency in &recipe.dependencies {
            variables.insert(
                format!("dep.{dependency}"),
                path_text(&self.artifact(dependency))?,
            );
        }
        for (alias, source) in &recipe.extra_sources {
            let archive = source::fetch(&self.sources.sources[source], &self.root.join("dl"))?;
            variables.insert(format!("extra.{alias}"), path_text(&archive)?);
        }
        Ok(variables)
    }

    fn initramfs(&self) -> Result<()> {
        let output = self.output.join("images/initramfs.cpio");
        let files = self
            .board
            .rootfs_files
            .iter()
            .chain(&self.profile.rootfs_files)
            .map(|(path, file)| {
                (
                    path.clone(),
                    self.output
                        .join("images")
                        .join(file.artifact(&self.recipes[file.package()])),
                    file.mode(),
                )
            })
            .collect::<Vec<_>>();
        match &self.profile.rootfs {
            Rootfs::Busybox { package, init } => {
                let init = config::input_path(&self.root, init)?;
                image::create_initramfs(
                    &self.artifact(package),
                    &init.join("init"),
                    &init.join("boot-check.sh"),
                    &files,
                    &output,
                )
            }
            Rootfs::Prebuilt {
                package,
                scripts,
                gzip_overlay,
            } => {
                fs::copy(self.artifact(package), &output)?;
                let scripts = scripts
                    .iter()
                    .map(|(name, path)| Ok((name.as_str(), config::input_path(&self.root, path)?)))
                    .collect::<Result<Vec<_>>>()?;
                image::append_scripts(&output, &scripts)?;
                image::append_files(&output, &files, *gzip_overlay)
            }
        }
    }

    fn inputs_digest(&self) -> Result<String> {
        let mut data = serde_json::to_vec(&(
            &self.preset,
            &self.board,
            &self.profile,
            &self.sources,
            &self.recipes,
        ))?;
        match &self.profile.rootfs {
            Rootfs::Busybox { init, .. } => {
                let directory = config::input_path(&self.root, init)?;
                for name in ["init", "boot-check.sh"] {
                    data.extend(fs::read(directory.join(name))?);
                }
            }
            Rootfs::Prebuilt { scripts, .. } => {
                for path in scripts.values() {
                    data.extend(fs::read(config::input_path(&self.root, path)?)?);
                }
            }
        }
        data.extend_from_slice(include_bytes!("image.rs"));
        data.extend_from_slice(include_bytes!("remote.rs"));
        data.extend_from_slice(include_bytes!("image/spacemit_k1.rs"));
        data.extend_from_slice(include_bytes!("engine.rs"));
        data.extend_from_slice(include_bytes!("config.rs"));
        data.extend_from_slice(include_bytes!("backend/mod.rs"));
        data.extend_from_slice(include_bytes!("backend/cargo.rs"));
        data.extend_from_slice(include_bytes!("backend/kbuild.rs"));
        data.extend_from_slice(include_bytes!("backend/prebuilt.rs"));
        data.extend_from_slice(include_bytes!("backend/go.rs"));
        for name in self.recipes.keys() {
            if let Some(path) = &self.recipes[name].config {
                data.extend(fs::read(config::input_path(&self.root, path)?)?);
            }
            for patch in self.patch_paths(name)? {
                data.extend(fs::read(patch)?);
            }
            for input in &self.recipes[name].inputs {
                data.extend(fs::read(config::input_path(&self.root, input)?)?);
            }
        }
        Ok(hash(data))
    }

    pub fn run(&self, timeout: Option<Duration>, qemu: Option<&str>) -> Result<()> {
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        self.run_inner(timeout, qemu)
    }

    pub fn test_boot(&self, timeout: Duration, qemu: Option<&str>) -> Result<()> {
        ensure!(
            self.board.qemu.is_some(),
            "test-boot requires a QEMU board; hardware boot must be verified through its serial console"
        );
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        self.build_inner(None, false)?;
        self.run_inner(Some(timeout), qemu)
    }

    fn run_inner(&self, timeout: Option<Duration>, qemu: Option<&str>) -> Result<()> {
        let q = self.board.qemu.as_ref().context(
            "this board boots on hardware; write its SD image to a card instead of running QEMU",
        )?;
        let manifest: Manifest = serde_json::from_slice(
            &fs::read(self.output.join("manifests/build.json"))
                .context("no complete system image; run forge build")?,
        )?;
        ensure!(
            manifest.inputs == self.inputs_digest()?,
            "configuration changed; run forge build"
        );
        let options = QemuOptions {
            program: qemu.unwrap_or(&q.program).into(),
            machine: q.machine.clone(),
            cpu: self
                .profile
                .boot
                .cpu
                .clone()
                .unwrap_or_else(|| q.cpu.clone()),
            memory: self
                .profile
                .boot
                .memory
                .clone()
                .unwrap_or_else(|| q.memory.clone()),
            smp: self.profile.boot.smp.unwrap_or(q.smp),
            args: self.profile.boot.qemu_args.clone(),
            boot_script: self.profile.boot.script.as_deref(),
            append: if timeout.is_some() {
                format!(
                    "{} {}",
                    self.profile.boot.append, self.profile.boot.test_append
                )
            } else {
                self.profile.boot.append.clone()
            },
        };
        image::run_qemu(
            &options,
            &self.artifact(&self.profile.firmware),
            self.profile
                .bootloader
                .as_ref()
                .map(|name| self.artifact(name))
                .as_deref(),
            &self.artifact(&self.profile.kernel),
            &self.output.join("images/initramfs.cpio"),
            &self.output.join("logs/qemu.log"),
            timeout,
        )
    }

    pub fn clean(&self) -> Result<()> {
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        if self.output.exists() {
            let output = self.output.canonicalize()?;
            let parent = self.root.join("output").canonicalize()?;
            ensure!(
                parent.starts_with(self.root.canonicalize()?) && output.parent() == Some(&parent),
                "refusing to clean outside configuration directory"
            );
            fs::remove_dir_all(&output)?;
            fs::create_dir_all(&output)?;
            publish(
                &output.join(".config.toml"),
                toml::to_string_pretty(&self.preset)?.as_bytes(),
            )?;
        }
        Ok(())
    }
}
