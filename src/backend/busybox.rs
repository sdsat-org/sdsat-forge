use super::builder::{Config, Kbuild};
use anyhow::Result;
use std::path::{Path, PathBuf};

const PACKAGE: Kbuild = Kbuild {
    name: "BusyBox",
    tools: &[],
    config: Config::Defconfig,
    args: &[
        "EXTRA_CFLAGS=-march=rv64gc -mabi=lp64d -Os",
        "EXTRA_LDFLAGS=-march=rv64gc -mabi=lp64d",
    ],
    artifact: "busybox",
};

pub fn check_host() -> Result<()> {
    PACKAGE.check_host()
}

pub fn build(source: &Path, toolchain: &Path, config: &Path, out: &Path) -> Result<PathBuf> {
    PACKAGE.build(source, toolchain, config, out)
}
