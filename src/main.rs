use anyhow::{Context, Result, bail, ensure};
use clap::{CommandFactory, Parser, Subcommand};
use sdsat_forge::{
    config,
    engine::{Build, WorkspaceGuard},
    process,
};
use std::{
    fs,
    path::PathBuf,
    process::{Command, ExitCode},
    time::Duration,
};

#[derive(Parser)]
#[command(
    version,
    about = "Build and boot software-defined satellite system stacks"
)]
struct Cli {
    #[arg(long, global = true, default_value = ".")]
    root: PathBuf,
    #[arg(long, global = true)]
    config: Option<String>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// List versioned system presets.
    ListDefconfigs,
    /// Select a preset and reset its active configuration.
    Defconfig { name: String },
    /// Display the resolved dependency graph without downloading or compiling.
    Plan,
    /// Fetch and verify all pinned source archives (Cargo dependencies are fetched during builds).
    Source,
    /// Build the full stack or one package and its dependencies.
    Build { package: Option<String> },
    /// Reinvoke the selected package backend and refresh its exported artifact.
    Rebuild { package: String },
    /// Boot the generated system image.
    Run {
        #[arg(long)]
        qemu: Option<String>,
    },
    /// Build, boot and require a userspace success marker followed by normal QEMU exit.
    TestBoot {
        #[arg(long, default_value_t = 120)]
        timeout: u64,
        #[arg(long)]
        qemu: Option<String>,
    },
    /// Inspect local programs and the profile Rust toolchain.
    Doctor,
    /// Install the profile Rust toolchain and helpers declared by the packages.
    Setup,
    /// Save the active configuration as a new preset, retaining its source lock reference.
    Savedefconfig { name: String },
    /// Remove this configuration's build outputs, retaining its config and the download cache.
    Clean,
}

fn main() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<()> {
    if !cli.root.join("forge.toml").is_file()
        && !matches!(
            cli.command,
            Action::ListDefconfigs | Action::Defconfig { .. }
        )
    {
        Cli::command().bin_name("forge").print_help()?;
        println!();
        println!("Select a preset with: just defconfig <name>");
        return Ok(());
    }
    let root = config::absolute(&cli.root)
        .context("locating repository; pass --root PATH when outside the repository")?;
    match &cli.command {
        Action::ListDefconfigs => {
            let mut names = Vec::new();
            for entry in fs::read_dir(root.join("configs"))? {
                let entry = entry?;
                if let Some(name) = entry
                    .file_name()
                    .to_string_lossy()
                    .strip_suffix("_defconfig.toml")
                {
                    names.push(name.to_owned());
                }
            }
            names.sort();
            for name in names {
                println!("{name}");
            }
            return Ok(());
        }
        Action::Defconfig { name } => return Build::select(&root, name),
        _ => (),
    }
    let build = Build::load(&root, cli.config.as_deref())?;
    match cli.command {
        Action::Plan => build.plan(),
        Action::Source => {
            let _guard = WorkspaceGuard::acquire(&root, "downloads")?;
            build.fetch_all()
        }
        Action::Build { package } => build.build(package.as_deref(), false),
        Action::Rebuild { package } => build.build(Some(&package), true),
        Action::Run { qemu } => build.run(None, qemu.as_deref()),
        Action::TestBoot { timeout, qemu } => {
            ensure!(timeout > 0, "timeout must be positive");
            build.test_boot(Duration::from_secs(timeout), qemu.as_deref())
        }
        Action::Clean => build.clean(),
        Action::Doctor => doctor(&build),
        Action::Setup => setup(&build),
        Action::Savedefconfig { name } => {
            config::validate_name(&name)?;
            let path = root.join(format!("configs/{name}_defconfig.toml"));
            ensure!(
                !path.exists(),
                "{} already exists; choose a new preset name",
                path.display()
            );
            let mut preset = build.preset.clone();
            preset.name = name;
            fs::write(&path, toml::to_string_pretty(&preset)?)?;
            println!("Saved {}", path.display());
            Ok(())
        }
        Action::ListDefconfigs | Action::Defconfig { .. } => unreachable!(),
    }
}

fn doctor(build: &Build) -> Result<()> {
    let mut missing = Vec::new();
    for program in ["cargo", "rustup", "curl", "git"]
        .into_iter()
        .chain(build.host_tools())
        .chain(build.board.qemu.as_ref().map(|q| q.program.as_str()))
    {
        match Command::new(program).arg("--version").output() {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout);
                println!(
                    "[ok] {program}: {}",
                    text.lines().next().unwrap_or("available")
                );
            }
            _ => {
                println!("[missing] {program}");
                missing.push(program.to_owned());
            }
        }
    }
    let toolchain = &build.profile.toolchain;
    match Command::new("rustup")
        .args(["run", toolchain, "rustc", "--version"])
        .output()
    {
        Ok(output) if output.status.success() => {
            println!("[ok] {}", String::from_utf8_lossy(&output.stdout).trim())
        }
        _ => {
            println!("[missing] {toolchain}; run forge setup");
            missing.push(toolchain.clone());
        }
    }
    build.check_host()?;
    build.plan()?;
    if !missing.is_empty() {
        bail!("missing requirements: {}", missing.join(", "));
    }
    Ok(())
}

fn setup(build: &Build) -> Result<()> {
    let targets = build.rust_targets().join(",");
    let components = build.rust_components().join(",");
    let mut args = vec![
        "toolchain",
        "install",
        &build.profile.toolchain,
        "--profile",
        "minimal",
    ];
    if !components.is_empty() {
        args.extend(["--component", &components]);
    }
    if !targets.is_empty() {
        args.extend(["--target", &targets]);
    }
    process::run("rustup", args, &build.root)?;
    for toolchain in build
        .cargo_tools()
        .iter()
        .map(|tool| &tool.toolchain)
        .collect::<std::collections::BTreeSet<_>>()
    {
        if toolchain != &build.profile.toolchain {
            process::run(
                "rustup",
                ["toolchain", "install", toolchain, "--profile", "minimal"],
                &build.root,
            )?;
        }
    }
    for tool in build.cargo_tools() {
        process::run(
            "cargo",
            [
                &format!("+{}", tool.toolchain),
                "install",
                "--locked",
                &tool.name,
                "--version",
                &tool.version,
            ],
            &build.root,
        )?;
    }
    Ok(())
}
