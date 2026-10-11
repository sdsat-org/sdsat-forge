use anyhow::{Context, Result, bail};
use std::{ffi::OsStr, path::Path, process::Command};

pub fn isolate_rust(command: &mut Command) {
    for variable in [
        "RUSTFLAGS",
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTUP_TOOLCHAIN",
        "RUSTC",
        "RUSTC_WRAPPER",
        "RUSTC_WORKSPACE_WRAPPER",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
    ] {
        command.env_remove(variable);
    }
}

pub fn logged(command: &mut Command, log: &Path) -> Result<()> {
    use std::{
        fs,
        io::{IsTerminal, Read, Write},
        process::Stdio,
        sync::{Arc, Mutex},
        thread,
    };
    // Cargo sees our pipes, so preserve the terminal's color support explicitly.
    // The environment also reaches Cargo processes spawned by upstream xtasks.
    if std::io::stderr().is_terminal()
        && std::env::var_os("CARGO_TERM_COLOR").is_none()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var("TERM").as_deref() != Ok("dumb")
    {
        command.env("CARGO_TERM_COLOR", "always");
    }
    if let Some(parent) = log.parent() {
        fs::create_dir_all(parent)?;
    }
    let output = Arc::new(Mutex::new(fs::File::create(log)?));
    eprintln!("[forge] {:?} (log: {})", command, log.display());
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("starting {:?}", command.get_program()))?;
    fn pump(
        mut pipe: impl Read + Send + 'static,
        output: Arc<Mutex<fs::File>>,
    ) -> thread::JoinHandle<std::io::Result<()>> {
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                let n = pipe.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                output.lock().unwrap().write_all(&buf[..n])?;
                std::io::stderr().write_all(&buf[..n])?;
            }
            Ok(())
        })
    }
    let stdout = pump(child.stdout.take().unwrap(), output.clone());
    let stderr = pump(child.stderr.take().unwrap(), output);
    let status = child.wait()?;
    stdout
        .join()
        .map_err(|_| anyhow::anyhow!("stdout logger panicked"))??;
    stderr
        .join()
        .map_err(|_| anyhow::anyhow!("stderr logger panicked"))??;
    if !status.success() {
        bail!("command failed with {status}; see {}", log.display());
    }
    Ok(())
}

pub fn run<I, S>(program: &str, args: I, cwd: &Path) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<_> = args.into_iter().collect();
    eprintln!(
        "[forge] {program} {}",
        args.iter()
            .map(|x| x.as_ref().to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    );
    let status = Command::new(program)
        .args(&args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("starting {program} in {}", cwd.display()))?;
    if !status.success() {
        bail!("{program} failed with {status}");
    }
    Ok(())
}
