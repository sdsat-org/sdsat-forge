use super::builder::{Config, Kbuild};
use anyhow::Result;
use std::path::{Path, PathBuf};

const PACKAGE: Kbuild = Kbuild {
    name: "Linux",
    tools: &["bash", "flex", "bison", "bc", "perl"],
    config: Config::Olddefconfig,
    args: &["Image"],
    artifact: "arch/riscv/boot/Image",
};

pub fn check_host() -> Result<()> {
    PACKAGE.check_host()
}

pub fn build(source: &Path, toolchain: &Path, config: &Path, out: &Path) -> Result<PathBuf> {
    PACKAGE.build(source, toolchain, config, out)
}
