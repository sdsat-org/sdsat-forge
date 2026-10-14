//! Assemble the vendor K1 SD boot layout without host image-building tools.

use anyhow::{Context, Result, ensure};
use fatfs::{FatType, FileSystem, FormatVolumeOptions, FsOptions};
use gptman::{GPT, GPTPartitionEntry};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::Path;
use tempfile::NamedTempFile;
use vm_fdt::FdtWriter;

pub const ARTIFACTS: &[&str] = &["fw_dynamic.itb", "env_k1-x.txt", "sdcard.img"];

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const BOOTFS_SIZE: u64 = 256 * MIB;
// On-disk GUID byte order (the first three fields are little-endian).
const LINUX_DATA: [u8; 16] = [
    0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x72, 0x47, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d, 0xe4,
];
const BASIC_DATA: [u8; 16] = [
    0xa2, 0xa0, 0xd0, 0xeb, 0xe5, 0xb9, 0x33, 0x44, 0x87, 0xc0, 0x68, 0xb6, 0xb7, 0x26, 0x99, 0xc7,
];

pub fn create(
    images: &Path,
    firmware: &Path,
    kernel: &Path,
    initramfs: &Path,
    bootargs: &str,
    dtb: &str,
) -> Result<()> {
    crate::config::validate_filename(dtb)?;
    ensure!(
        !bootargs.contains(['\n', '\r', '\0']),
        "SD boot arguments must fit on one line"
    );
    let firmware = fs::read(firmware)?;
    ensure!(!firmware.is_empty(), "RustSBI binary is empty");
    let fit = firmware_fit(&firmware)?;
    ensure!(
        fit.len() <= 384 * KIB as usize,
        "RustSBI FIT exceeds the 384 KiB SBI partition"
    );
    let bootinfo = fs::read(images.join("bootinfo_sd.bin"))?;
    ensure!(
        bootinfo.len() == 80,
        "K1 SD bootinfo must contain exactly 80 bytes"
    );

    let environment = format!(
        "bootdelay=1\n\
         kernel_addr_r=0x08000000\n\
         ramdisk_addr=0x21000000\n\
         dtb_addr=0x31000000\n\
         bootargs={bootargs}\n\
         forge_kernel=load ${{bootfs_devname}} ${{boot_devnum}}:${{bootfs_part}} ${{kernel_addr_r}} Image\n\
         forge_dtb=load ${{bootfs_devname}} ${{boot_devnum}}:${{bootfs_part}} ${{dtb_addr}} {dtb}\n\
         forge_initrd=load ${{bootfs_devname}} ${{boot_devnum}}:${{bootfs_part}} ${{ramdisk_addr}} initramfs-generic.img && setenv initrd_size ${{filesize}}\n\
         bootcmd=run forge_kernel && run forge_dtb && run forge_initrd && booti ${{kernel_addr_r}} ${{ramdisk_addr}}:${{initrd_size}} ${{dtb_addr}}\n"
    );
    let dtb_path = images.join(dtb);
    let mut bootfs = NamedTempFile::new_in(images)?;
    bootfs.as_file().set_len(BOOTFS_SIZE)?;
    fatfs::format_volume(
        &mut bootfs,
        FormatVolumeOptions::new()
            .fat_type(FatType::Fat16)
            .volume_label(*b"FORGEBOOT  "),
    )?;
    bootfs.rewind()?;
    let volume = FileSystem::new(&mut bootfs, FsOptions::new())?;
    {
        let root = volume.root_dir();
        for (name, path) in [
            ("Image", kernel),
            ("initramfs-generic.img", initramfs),
            (dtb, dtb_path.as_path()),
        ] {
            io::copy(&mut File::open(path)?, &mut root.create_file(name)?)
                .with_context(|| format!("add {} to SD boot filesystem", path.display()))?;
        }
        root.create_file("env_k1-x.txt")?
            .write_all(environment.as_bytes())?;
    }
    volume.unmount()?;

    let mut sbi = NamedTempFile::new_in(images)?;
    sbi.write_all(&fit)?;
    let layout = [
        (
            "fsbl",
            images.join("FSBL.bin"),
            128 * KIB,
            256 * KIB,
            LINUX_DATA,
        ),
        (
            "env",
            images.join("u-boot-env-default.bin"),
            384 * KIB,
            64 * KIB,
            LINUX_DATA,
        ),
        (
            "opensbi",
            sbi.path().to_owned(),
            448 * KIB,
            384 * KIB,
            LINUX_DATA,
        ),
        (
            "uboot",
            images.join("u-boot.itb"),
            832 * KIB,
            2 * MIB,
            LINUX_DATA,
        ),
        (
            "bootfs",
            bootfs.path().to_owned(),
            4 * MIB,
            BOOTFS_SIZE,
            BASIC_DATA,
        ),
    ];
    for (name, path, _, capacity, _) in &layout {
        let size = path
            .metadata()
            .with_context(|| format!("read {}", path.display()))?
            .len();
        ensure!(
            size > 0 && size <= *capacity,
            "{name} image is {size} bytes; partition capacity is {capacity}"
        );
    }
    let mut disk = NamedTempFile::new_in(images)?;
    // Keep one MiB after bootfs for the backup GPT.
    disk.as_file().set_len(261 * MIB)?;
    let mut gpt = GPT::new_from(&mut disk, 512, guid("disk"))?;
    for (index, (name, path, offset, capacity, kind)) in layout.iter().enumerate() {
        gpt[index as u32 + 1] = GPTPartitionEntry {
            partition_type_guid: *kind,
            unique_partition_guid: guid(name),
            starting_lba: offset / 512,
            ending_lba: (offset + capacity) / 512 - 1,
            attribute_bits: 0,
            partition_name: (*name).into(),
        };
        disk.seek(SeekFrom::Start(*offset))?;
        io::copy(&mut File::open(path)?, &mut disk)?;
    }
    gpt.write_into(&mut disk)?;
    GPT::write_protective_mbr_into(&mut disk, 512)?;
    // BootROM reads only the first 80 bytes; preserve the protective MBR at 446.
    disk.rewind()?;
    disk.write_all(&bootinfo)?;
    disk.flush()?;
    sbi.persist(images.join(ARTIFACTS[0]))?;
    fs::write(images.join(ARTIFACTS[1]), environment)?;
    disk.persist(images.join(ARTIFACTS[2]))?;
    Ok(())
}

fn firmware_fit(firmware: &[u8]) -> Result<Vec<u8>> {
    let mut fdt = FdtWriter::new()?;
    let root = fdt.begin_node("")?;
    fdt.property_string("description", "RustSBI dynamic firmware")?;
    fdt.property_u32("#address-cells", 2)?;
    let images = fdt.begin_node("images")?;
    let sbi = fdt.begin_node("opensbi")?;
    for (key, value) in [
        ("description", "RustSBI"),
        ("type", "firmware"),
        ("os", "opensbi"),
        ("arch", "riscv"),
        ("compression", "none"),
    ] {
        fdt.property_string(key, value)?;
    }
    fdt.property("data", firmware)?;
    fdt.property_u64("load", 0x10000)?;
    fdt.property_u64("entry", 0x10000)?;
    let hash = fdt.begin_node("hash-1")?;
    fdt.property_string("algo", "crc32")?;
    fdt.property_u32("value", crc32fast::hash(firmware))?;
    fdt.end_node(hash)?;
    fdt.end_node(sbi)?;
    fdt.end_node(images)?;
    let configurations = fdt.begin_node("configurations")?;
    fdt.property_string("default", "config_1")?;
    let config = fdt.begin_node("config_1")?;
    fdt.property_string("description", "opensbi FIT config")?;
    fdt.property_string("firmware", "opensbi")?;
    fdt.end_node(config)?;
    fdt.end_node(configurations)?;
    fdt.end_node(root)?;
    Ok(fdt.finish()?)
}

fn guid(name: &str) -> [u8; 16] {
    // Stable image identifiers keep repeated builds reproducible.
    let digest = Sha256::digest(format!("sdsat-forge:k1:{name}"));
    let mut guid = [0; 16];
    guid.copy_from_slice(&digest[..16]);
    guid[7] = (guid[7] & 0x0f) | 0x40;
    guid[8] = (guid[8] & 0x3f) | 0x80;
    guid
}
