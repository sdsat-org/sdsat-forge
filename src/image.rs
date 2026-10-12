//! Package initramfs images and boot them with QEMU.

use anyhow::{Context, Result, ensure};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Package the BusyBox shell environment without creating host device nodes or
/// symlinks. Normalize script line endings for Windows source checkouts.
pub fn create_initramfs(busybox: &Path, init: &Path, check: &Path, output: &Path) -> Result<()> {
    let binary = fs::read(busybox).with_context(|| format!("read {}", busybox.display()))?;
    let init = fs::read_to_string(init)
        .with_context(|| format!("read {}", init.display()))?
        .replace("\r\n", "\n");
    let check = fs::read_to_string(check)
        .with_context(|| format!("read {}", check.display()))?
        .replace("\r\n", "\n");
    let mut archive = Vec::new();
    for (index, (name, mode, device, data)) in [
        (".", 0o040755, (0, 0), &[][..]),
        ("bin", 0o040755, (0, 0), &[][..]),
        ("sbin", 0o040755, (0, 0), &[][..]),
        ("usr", 0o040755, (0, 0), &[][..]),
        ("usr/bin", 0o040755, (0, 0), &[][..]),
        ("usr/sbin", 0o040755, (0, 0), &[][..]),
        ("dev", 0o040755, (0, 0), &[][..]),
        ("proc", 0o040755, (0, 0), &[][..]),
        ("sys", 0o040755, (0, 0), &[][..]),
        ("tmp", 0o041777, (0, 0), &[][..]),
        ("root", 0o040755, (0, 0), &[][..]),
        ("etc", 0o040755, (0, 0), &[][..]),
        ("bin/busybox", 0o100755, (0, 0), binary.as_slice()),
        ("bin/sh", 0o120777, (0, 0), b"busybox".as_slice()),
        ("init", 0o100755, (0, 0), init.as_bytes()),
        ("boot-check.sh", 0o100755, (0, 0), check.as_bytes()),
        (
            "etc/passwd",
            0o100644,
            (0, 0),
            b"root:x:0:0:root:/root:/bin/sh\n".as_slice(),
        ),
        ("etc/group", 0o100644, (0, 0), b"root:x:0:\n".as_slice()),
        ("dev/console", 0o020600, (5, 1), &[][..]),
        ("dev/null", 0o020666, (1, 3), &[][..]),
        ("TRAILER!!!", 0, (0, 0), &[][..]),
    ]
    .into_iter()
    .enumerate()
    {
        append_entry(&mut archive, index as u32 + 1, name, mode, device, data)?;
    }
    pad(&mut archive, 512);
    if let Some(parent) = output.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, archive).with_context(|| format!("write {}", output.display()))
}

/// Linux accepts concatenated compressed and uncompressed newc archives.
/// Append executable support scripts without unpacking a vendor rootfs on the host.
pub fn append_scripts(output: &Path, scripts: &[(&str, PathBuf)]) -> Result<()> {
    if scripts.is_empty() {
        return Ok(());
    }
    let mut archive = Vec::new();
    for (index, (name, path)) in scripts.iter().enumerate() {
        crate::config::validate_filename(name)?;
        let script = fs::read_to_string(path)?.replace("\r\n", "\n");
        append_entry(
            &mut archive,
            index as u32 + 1,
            name,
            0o100755,
            (0, 0),
            script.as_bytes(),
        )?;
    }
    append_entry(&mut archive, 0, "TRAILER!!!", 0, (0, 0), &[])?;
    pad(&mut archive, 512);
    let mut file = fs::OpenOptions::new().append(true).open(output)?;
    // Each uncompressed newc member starts at a four-byte boundary.
    let padding = (4 - file.metadata()?.len() % 4) % 4;
    file.write_all(&[0; 3][..padding as usize])?;
    file.write_all(&archive)?;
    Ok(())
}

fn append_entry(
    archive: &mut Vec<u8>,
    inode: u32,
    name: &str,
    mode: u32,
    device: (u32, u32),
    data: &[u8],
) -> Result<()> {
    let size = u32::try_from(data.len()).context("entry exceeds newc's 4 GiB limit")?;
    let links = if mode & 0o170000 == 0o040000 { 2 } else { 1 };
    archive.extend_from_slice(b"070701");
    // ino, mode, uid, gid, nlink, mtime, size, dev, rdev, namesize, checksum.
    for value in [
        inode,
        mode,
        0,
        0,
        links,
        0,
        size,
        0,
        0,
        device.0,
        device.1,
        name.len() as u32 + 1,
        0,
    ] {
        write!(archive, "{value:08x}")?;
    }
    archive.extend_from_slice(name.as_bytes());
    archive.push(0);
    pad(archive, 4);
    archive.extend_from_slice(data);
    pad(archive, 4);
    Ok(())
}

fn pad(bytes: &mut Vec<u8>, alignment: usize) {
    let count = (alignment - bytes.len() % alignment) % alignment;
    bytes.resize(bytes.len() + count, 0);
}

#[derive(Debug, Clone)]
pub struct QemuOptions {
    pub program: String,
    pub machine: String,
    pub cpu: String,
    pub memory: String,
    pub smp: u32,
    pub append: String,
}

/// A timed boot check requires a complete readiness line and successful exit.
/// Both modes retain stdout/stderr in the log and display output as it arrives.
pub fn run_qemu(
    options: &QemuOptions,
    firmware: &Path,
    bootloader: Option<&Path>,
    kernel: &Path,
    initramfs: &Path,
    log: &Path,
    timeout: Option<Duration>,
) -> Result<()> {
    ensure!(options.smp > 0, "QEMU requires at least one CPU");
    for path in [firmware, kernel, initramfs].into_iter().chain(bootloader) {
        ensure!(path.is_file(), "missing QEMU input: {}", path.display());
    }
    if let Some(parent) = log.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    let log_file = Arc::new(Mutex::new(File::create(log)?));
    // Keep the read-only boot volume alive until QEMU and its log readers exit.
    let boot_files = bootloader
        .map(|_| boot_directory(kernel, initramfs, &options.append))
        .transpose()?;
    let mut command = Command::new(&options.program);
    command
        .args([
            "-machine",
            &options.machine,
            "-cpu",
            &options.cpu,
            "-m",
            &options.memory,
            "-smp",
            &options.smp.to_string(),
        ])
        .arg("-bios")
        .arg(firmware);
    if let Some(bootloader) = bootloader {
        // QEMU supplies the board DTB; U-Boot loads the OS from a VirtIO FAT disk.
        // JSON preserves host paths, including Windows drive letters and commas.
        let disk = serde_json::json!({
            "driver": "vvfat",
            "node-name": "forge-boot",
            "dir": boot_files.as_ref().unwrap().path(),
            "fat-type": 16,
            "read-only": true,
            "rw": false,
        });
        command
            .arg("-kernel")
            .arg(bootloader)
            .arg("-blockdev")
            .arg(serde_json::to_string(&disk)?)
            .args(["-device", "virtio-blk-device,drive=forge-boot"]);
    } else {
        command
            .arg("-kernel")
            .arg(kernel)
            .arg("-initrd")
            .arg(initramfs)
            .args(["-append", &options.append]);
    }
    let mut child = command
        .args(["-nographic", "-no-reboot"])
        .stdin(if timeout.is_some() {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("start {}", options.program))?;
    // Drain concurrently before waiting, so neither pipe can fill and deadlock.
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let stdout_log = Arc::clone(&log_file);
    let output = thread::spawn(move || drain(stdout, stdout_log, false));
    let errors = thread::spawn(move || drain(stderr, log_file, true));
    let status = wait_for_exit(&mut child, timeout);
    // Always join both readers, including after timeout, to finish the log.
    let output = output
        .join()
        .map_err(|_| anyhow::anyhow!("stdout logger panicked"));
    let errors = errors
        .join()
        .map_err(|_| anyhow::anyhow!("stderr logger panicked"));
    let marker = output??;
    errors??;
    let status = status.with_context(|| format!("QEMU failed; see {}", log.display()))?;
    ensure!(
        status.success(),
        "QEMU exited with {status}; see {}",
        log.display()
    );
    ensure!(
        timeout.is_none() || marker,
        "QEMU exited without FORGE_BOOT_OK; see {}",
        log.display()
    );
    Ok(())
}

/// Stage an extlinux boot volume without host filesystem tools or symlinks.
fn boot_directory(kernel: &Path, initramfs: &Path, append: &str) -> Result<tempfile::TempDir> {
    ensure!(
        !append.contains(['\n', '\r']),
        "boot arguments must fit on one line"
    );
    let directory = tempfile::Builder::new()
        .prefix(".uboot-")
        .tempdir_in(kernel.parent().context("kernel needs a parent directory")?)?;
    let root = directory.path();
    fs::copy(kernel, root.join("Image"))?;
    fs::copy(initramfs, root.join("initramfs.cpio"))?;
    fs::create_dir(root.join("extlinux"))?;
    fs::write(
        root.join("extlinux/extlinux.conf"),
        format!(
            "default forge\ntimeout 0\nlabel forge\n  linux /Image\n  initrd /initramfs.cpio\n  append {append}\n"
        ),
    )?;
    Ok(directory)
}

fn wait_for_exit(child: &mut Child, timeout: Option<Duration>) -> Result<ExitStatus> {
    let started = Instant::now();
    let result = (|| loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        ensure!(
            !timeout.is_some_and(|limit| started.elapsed() >= limit),
            "QEMU boot timed out"
        );
        thread::sleep(Duration::from_millis(20));
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

const BOOT_MARKER: &[u8] = b"FORGE_BOOT_OK";

#[derive(Default)]
struct BootMarker {
    line: Vec<u8>,
    found: bool,
}

impl BootMarker {
    fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' {
                self.found |= self.line.strip_suffix(b"\r").unwrap_or(&self.line) == BOOT_MARKER;
                self.line.clear();
            } else if self.line.len() <= BOOT_MARKER.len() + 1 {
                // One byte beyond a possible CRLF marker rejects longer lines
                // while arbitrary guest output stays bounded in memory.
                self.line.push(byte);
            }
        }
    }
}

fn drain(mut reader: impl Read, log: Arc<Mutex<File>>, stderr: bool) -> Result<bool> {
    let mut buffer = [0; 8192];
    let mut marker = BootMarker::default();
    let mut write_error = None;
    loop {
        let size = match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(size) => size,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error).context("read QEMU output"),
        };
        let bytes = &buffer[..size];
        if let Err(error) = log
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .write_all(bytes)
        {
            // Continue draining even if the log disk is full.
            write_error.get_or_insert(error);
        }
        if stderr {
            let _ = io::stderr().write_all(bytes);
        } else {
            let _ = io::stdout().write_all(bytes);
            let _ = io::stdout().flush();
        }
        marker.observe(bytes);
    }
    if let Some(error) = write_error {
        return Err(error).context("write QEMU log");
    }
    Ok(marker.found)
}
