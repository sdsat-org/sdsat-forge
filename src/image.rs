//! Package system images and boot them with QEMU.

pub mod spacemit_k1;

use anyhow::{Context, Result, ensure};
use flate2::{Compression, write::GzEncoder};
use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

/// Package the BusyBox shell environment without creating host device nodes or
/// symlinks. Normalize script line endings for Windows source checkouts.
/// Include extra files before the trailer for readers that stop after one archive.
pub fn create_initramfs(
    busybox: &Path,
    init: &Path,
    check: &Path,
    files: &[(String, PathBuf, u32)],
    output: &Path,
) -> Result<()> {
    let binary = fs::read(busybox).with_context(|| format!("read {}", busybox.display()))?;
    let init = fs::read_to_string(init)
        .with_context(|| format!("read {}", init.display()))?
        .replace("\r\n", "\n");
    let check = fs::read_to_string(check)
        .with_context(|| format!("read {}", check.display()))?
        .replace("\r\n", "\n");
    let mut archive = Vec::new();
    let mut directories = BTreeSet::new();
    let mut inode = 1;
    for (name, mode, device, data) in [
        (".", 0o040755, (0, 0), &[][..]),
        ("bin", 0o040755, (0, 0), &[][..]),
        ("sbin", 0o040755, (0, 0), &[][..]),
        ("usr", 0o040755, (0, 0), &[][..]),
        ("usr/bin", 0o040755, (0, 0), &[][..]),
        ("usr/sbin", 0o040755, (0, 0), &[][..]),
        ("usr/libexec", 0o040755, (0, 0), &[][..]),
        ("usr/libexec/tianzhi", 0o040755, (0, 0), &[][..]),
        ("dev", 0o040755, (0, 0), &[][..]),
        ("proc", 0o040755, (0, 0), &[][..]),
        ("sys", 0o040755, (0, 0), &[][..]),
        ("tmp", 0o041777, (0, 0), &[][..]),
        ("root", 0o040755, (0, 0), &[][..]),
        ("etc", 0o040755, (0, 0), &[][..]),
        ("bin/busybox", 0o100755, (0, 0), binary.as_slice()),
        ("bin/sh", 0o120777, (0, 0), b"busybox".as_slice()),
        ("init", 0o100755, (0, 0), init.as_bytes()),
        (
            "usr/libexec/tianzhi/boot-check",
            0o100755,
            (0, 0),
            check.as_bytes(),
        ),
        (
            "etc/passwd",
            0o100644,
            (0, 0),
            b"root:x:0:0:root:/root:/bin/sh\n".as_slice(),
        ),
        ("etc/group", 0o100644, (0, 0), b"root:x:0:\n".as_slice()),
        ("dev/console", 0o020600, (5, 1), &[][..]),
        ("dev/null", 0o020666, (1, 3), &[][..]),
    ] {
        if mode & 0o170000 == 0o040000 {
            directories.insert(name.to_owned());
        }
        append_entry(&mut archive, inode, name, mode, device, data)?;
        inode += 1;
    }
    append_file_entries(&mut archive, files, &mut directories, &mut inode)?;
    append_entry(&mut archive, inode, "TRAILER!!!", 0, (0, 0), &[])?;
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
    let mut directories = BTreeSet::new();
    let mut inode = 1;
    for (name, path) in scripts {
        append_parent_directories(&mut archive, name, &mut directories, &mut inode)?;
        let script = fs::read_to_string(path)?.replace("\r\n", "\n");
        append_entry(
            &mut archive,
            inode,
            name,
            0o100755,
            (0, 0),
            script.as_bytes(),
        )?;
        inode += 1;
    }
    append_archive(output, &mut archive, false)
}

/// Append another newc archive to a prebuilt rootfs, retaining its existing bytes.
pub fn append_files(
    output: &Path,
    files: &[(String, PathBuf, u32)],
    gzip_overlay: bool,
) -> Result<()> {
    if files.is_empty() {
        return Ok(());
    }
    let mut archive = Vec::new();
    let mut directories = BTreeSet::new();
    let mut inode = 1;
    append_file_entries(&mut archive, files, &mut directories, &mut inode)?;
    append_archive(output, &mut archive, gzip_overlay)
}

fn append_file_entries(
    archive: &mut Vec<u8>,
    files: &[(String, PathBuf, u32)],
    directories: &mut BTreeSet<String>,
    inode: &mut u32,
) -> Result<()> {
    for (name, path, mode) in files {
        append_parent_directories(archive, name, directories, inode)?;
        append_entry(
            archive,
            *inode,
            name,
            0o100000 | (mode & 0o777),
            (0, 0),
            &fs::read(path).with_context(|| format!("read rootfs file {}", path.display()))?,
        )?;
        *inode += 1;
    }
    Ok(())
}

fn append_parent_directories(
    archive: &mut Vec<u8>,
    name: &str,
    directories: &mut BTreeSet<String>,
    inode: &mut u32,
) -> Result<()> {
    let parts = name.split('/').collect::<Vec<_>>();
    for part in &parts {
        crate::config::validate_filename(part)?;
    }
    let mut directory = String::new();
    for part in &parts[..parts.len() - 1] {
        directory.push_str(part);
        if directories.insert(directory.clone()) {
            append_entry(archive, *inode, &directory, 0o040755, (0, 0), &[])?;
            *inode += 1;
        }
        directory.push('/');
    }
    Ok(())
}

fn append_archive(output: &Path, archive: &mut Vec<u8>, gzip: bool) -> Result<()> {
    append_entry(archive, 0, "TRAILER!!!", 0, (0, 0), &[])?;
    pad(archive, 512);
    let mut file = fs::OpenOptions::new().append(true).open(output)?;
    // Each uncompressed newc member starts at a four-byte boundary.
    let padding = (4 - file.metadata()?.len() % 4) % 4;
    file.write_all(&[0; 3][..padding as usize])?;
    if gzip {
        let mut encoder = GzEncoder::new(file, Compression::fast());
        encoder.write_all(archive)?;
        encoder.finish()?;
    } else {
        file.write_all(archive)?;
    }
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
pub struct QemuOptions<'a> {
    pub program: String,
    pub machine: String,
    pub cpu: String,
    pub memory: String,
    pub smp: u32,
    pub append: String,
    pub args: Vec<String>,
    pub boot_script: Option<&'a str>,
}

/// A timed boot check requires a complete readiness line and successful exit.
/// Both modes retain stdout/stderr in the log and display output as it arrives.
pub fn run_qemu(
    options: &QemuOptions<'_>,
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
        .map(|_| boot_directory(kernel, initramfs, &options.append, options.boot_script))
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
        .args(&options.args)
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
fn boot_directory(
    kernel: &Path,
    initramfs: &Path,
    append: &str,
    script: Option<&str>,
) -> Result<tempfile::TempDir> {
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
    // A configured script must run before extlinux, so keep its configuration
    // outside the paths automatically scanned by U-Boot's distro boot command.
    let config_directory = if script.is_some() { "boot" } else { "extlinux" };
    fs::create_dir(root.join(config_directory))?;
    fs::write(
        root.join(config_directory).join("extlinux.conf"),
        format!(
            "default forge\ntimeout 0\nlabel forge\n  linux /Image\n  initrd /initramfs.cpio\n  append {append}\n"
        ),
    )?;
    if let Some(script) = script {
        fs::write(root.join("boot.scr"), uboot_script(script)?)?;
    }
    Ok(directory)
}

/// Wrap configured commands in U-Boot's legacy script image format.
fn uboot_script(script: &str) -> Result<Vec<u8>> {
    ensure!(
        !script.trim().is_empty() && !script.contains('\0'),
        "U-Boot script must contain commands without NUL bytes"
    );
    let script = script.replace("\r\n", "\n");
    let length = u32::try_from(script.len()).context("U-Boot script is too large")?;
    let mut data = Vec::new();
    // Script images use a size table terminated by a zero-sized component.
    data.extend_from_slice(&length.to_be_bytes());
    data.extend_from_slice(&0u32.to_be_bytes());
    data.extend_from_slice(script.as_bytes());
    let mut header = [0u8; 64];
    header[..4].copy_from_slice(&0x27051956u32.to_be_bytes());
    header[12..16].copy_from_slice(
        &u32::try_from(data.len())
            .context("U-Boot script image is too large")?
            .to_be_bytes(),
    );
    header[24..28].copy_from_slice(&crc32fast::hash(&data).to_be_bytes());
    // Commands are architecture independent; source only requires type SCRIPT.
    header[30] = 6;
    header[32..49].copy_from_slice(b"SDSAT boot script");
    let header_crc = crc32fast::hash(&header);
    header[4..8].copy_from_slice(&header_crc.to_be_bytes());
    let mut image = header.to_vec();
    image.extend_from_slice(&data);
    Ok(image)
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
