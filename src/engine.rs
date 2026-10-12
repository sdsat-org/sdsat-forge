use crate::{
    config::{self, Board, Preset, Profile, Recipe, Rootfs},
    image::{self, QemuOptions},
    process,
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
    sha256: String,
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
            board.schema == 1 && board.arch == "riscv64" && board.qemu.smp > 0,
            "unsupported board schema/architecture/CPU count"
        );
        let profile: Profile = config::read(&config::input_path(
            &root,
            &format!("profiles/{}/profile.toml", preset.profile),
        )?)?;
        ensure!(profile.schema == 1, "unsupported profile schema");
        let sources = source::load_lock(&config::input_path(&root, &preset.lock)?)?;
        let recipes = config::recipes(&root, &preset)?;
        let mut artifacts = BTreeSet::from(["initramfs.cpio"]);
        for recipe in recipes.values() {
            ensure!(
                sources.sources.contains_key(&recipe.source),
                "unlocked source {}",
                recipe.source
            );
            ensure!(
                artifacts.insert(&recipe.artifact),
                "duplicate or reserved artifact {}",
                recipe.artifact
            );
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
            if crate::backend::kbuild(&recipe.backend).is_some() {
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
            if recipe.backend == "asterinas" {
                ensure!(
                    recipe.target.is_some(),
                    "{} needs a Rust target",
                    recipe.name
                );
            }
            ensure!(
                recipe.file.is_none() || recipe.backend == "prebuilt",
                "file is only supported for prebuilt packages"
            );
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
        if let Some(name) = &profile.bootloader {
            ensure!(
                recipes[name].backend == "uboot",
                "bootloader needs a U-Boot package"
            );
            ensure!(
                recipes[&profile.kernel].backend == "linux",
                "U-Boot boot currently needs a Linux Image"
            );
        }
        ensure!(
            profile.boot.smp.is_none_or(|smp| smp > 0),
            "CPU count must be positive"
        );
        match &profile.rootfs {
            Rootfs::Busybox { package, init } => {
                ensure!(
                    recipes[package].backend == "busybox",
                    "BusyBox rootfs needs a busybox package"
                );
                let directory = config::input_path(&root, init)?;
                for name in ["init", "boot-check.sh"] {
                    ensure!(
                        directory.join(name).is_file(),
                        "missing rootfs script {init}/{name}"
                    );
                }
            }
            Rootfs::Prebuilt { package, scripts } => {
                ensure!(
                    recipes[package].backend == "prebuilt",
                    "prebuilt rootfs needs a prebuilt package"
                );
                for (name, path) in scripts {
                    config::validate_filename(name)?;
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
        println!("  QEMU: {}", stages.join(" -> "));
        Ok(())
    }

    pub fn rust_targets(&self) -> Vec<&str> {
        std::iter::once(self.board.firmware_target.as_str())
            .chain(self.recipes.values().filter_map(|r| r.target.as_deref()))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect()
    }

    pub fn check_host(&self) -> Result<()> {
        let backends = self
            .recipes
            .values()
            .map(|recipe| recipe.backend.as_str())
            .collect::<BTreeSet<_>>();
        for name in backends {
            if let Some(backend) = crate::backend::kbuild(name) {
                backend.check_host()?;
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
        let mut artifacts = vec![
            self.recipes[&self.profile.firmware].artifact.as_str(),
            self.recipes[&self.profile.kernel].artifact.as_str(),
            "initramfs.cpio",
        ];
        if let Some(name) = &self.profile.bootloader {
            artifacts.push(&self.recipes[name].artifact);
        }
        artifacts
    }

    pub fn fetch_all(&self) -> Result<()> {
        for name in self.order(None)? {
            let recipe = &self.recipes[&name];
            for input in std::iter::once(&recipe.source).chain(recipe.toolchain.iter()) {
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
        match recipe.backend.as_str() {
            "asterinas" => data.extend_from_slice(include_bytes!("backend/asterinas.rs")),
            "busybox" => data.extend_from_slice(include_bytes!("backend/busybox.rs")),
            "linux" => data.extend_from_slice(include_bytes!("backend/linux.rs")),
            "uboot" => data.extend_from_slice(include_bytes!("backend/uboot.rs")),
            _ => (),
        }
        if crate::backend::kbuild(&recipe.backend).is_some() {
            data.extend_from_slice(include_bytes!("backend/builder.rs"));
            data.extend_from_slice(include_bytes!("backend/mod.rs"));
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
        data.extend(fs::read(config::input_path(
            &self.root,
            &self.board.firmware_config,
        )?)?);
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

    fn build_inner(&self, package: Option<&str>, force: bool) -> Result<()> {
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
            let source = if crate::backend::kbuild(&recipe.backend).is_some()
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
                s.fingerprint == fingerprint && digest_file(&artifact).is_ok_and(|d| d == s.sha256)
            });
            if cached && !(force && package == Some(name.as_str())) {
                println!("[cached] {name}");
                continue;
            }
            let work = self.output.join(format!("build/{name}-work"));
            if recipe.backend != "prebuilt" {
                safe_directory(&self.root, &work)?;
            }
            let built = if let Some(backend) = crate::backend::kbuild(&recipe.backend) {
                backend.build(
                    &source,
                    &source::fetch(
                        &self.sources.sources[recipe.toolchain.as_ref().unwrap()],
                        &self.root.join("dl"),
                    )?,
                    &config::input_path(&self.root, recipe.config.as_deref().unwrap())?,
                    &work,
                )?
            } else {
                match recipe.backend.as_str() {
                    "asterinas" => crate::backend::asterinas::build(
                        &source,
                        &self.output.join("images"),
                        &work,
                        &self.profile.toolchain,
                        recipe.target.as_deref().unwrap(),
                    )?,
                    "rustsbi" => self.firmware(&source, &work)?,
                    "prebuilt" => recipe
                        .file
                        .as_ref()
                        .map_or_else(|| source.clone(), |file| source.join(file)),
                    _ => unreachable!("validated backend"),
                }
            };
            if built != artifact {
                publish(&artifact, &fs::read(built)?)?;
            }
            let state = State {
                fingerprint,
                sha256: digest_file(&artifact)?,
            };
            publish(&state_path, &serde_json::to_vec(&state)?)?;
        }
        if package.is_none() {
            self.initramfs()?;
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

    fn firmware(&self, source: &Path, work: &Path) -> Result<PathBuf> {
        let target = work.join("target");
        let mut cmd = Command::new("cargo");
        process::isolate_rust(&mut cmd);
        cmd.arg(format!("+{}", self.profile.toolchain))
            .args(["prototyper", "build", "--config-file"])
            .arg(config::input_path(&self.root, &self.board.firmware_config)?)
            .args(["--target", &self.board.firmware_target, "dynamic"])
            .current_dir(source)
            .env("CARGO_TARGET_DIR", &target);
        process::logged(&mut cmd, &self.output.join("logs/rustsbi.log"))?;
        Ok(target
            .join(&self.board.firmware_target)
            .join("release/rustsbi-prototyper-dynamic.elf"))
    }

    fn initramfs(&self) -> Result<()> {
        let output = self.output.join("images/initramfs.cpio");
        match &self.profile.rootfs {
            Rootfs::Busybox { package, init } => {
                let init = config::input_path(&self.root, init)?;
                image::create_initramfs(
                    &self.artifact(package),
                    &init.join("init"),
                    &init.join("boot-check.sh"),
                    &output,
                )
            }
            Rootfs::Prebuilt { package, scripts } => {
                fs::copy(self.artifact(package), &output)?;
                let scripts = scripts
                    .iter()
                    .map(|(name, path)| Ok((name.as_str(), config::input_path(&self.root, path)?)))
                    .collect::<Result<Vec<_>>>()?;
                image::append_scripts(&output, &scripts)
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
        data.extend(fs::read(config::input_path(
            &self.root,
            &self.board.firmware_config,
        )?)?);
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
        for name in self.recipes.keys() {
            if let Some(path) = &self.recipes[name].config {
                data.extend(fs::read(config::input_path(&self.root, path)?)?);
            }
            for patch in self.patch_paths(name)? {
                data.extend(fs::read(patch)?);
            }
        }
        Ok(hash(data))
    }

    pub fn run(&self, timeout: Option<Duration>, qemu: Option<&str>) -> Result<()> {
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        self.run_inner(timeout, qemu)
    }

    pub fn test_boot(&self, timeout: Duration, qemu: Option<&str>) -> Result<()> {
        let _guard = WorkspaceGuard::acquire(&self.root, &self.preset.name)?;
        self.build_inner(None, false)?;
        self.run_inner(Some(timeout), qemu)
    }

    fn run_inner(&self, timeout: Option<Duration>, qemu: Option<&str>) -> Result<()> {
        let manifest: Manifest = serde_json::from_slice(
            &fs::read(self.output.join("manifests/build.json"))
                .context("no complete system image; run forge build")?,
        )?;
        ensure!(
            manifest.inputs == self.inputs_digest()?,
            "configuration changed; run forge build"
        );
        for name in self.system_artifacts() {
            ensure!(
                manifest.artifacts.get(name)
                    == Some(&digest_file(&self.output.join("images").join(name))?),
                "artifact {name} changed; run forge build"
            );
        }
        let q = &self.board.qemu;
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
