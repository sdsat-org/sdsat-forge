use super::builder::{Config, Kbuild};

pub(super) const PACKAGE: Kbuild = Kbuild {
    name: "BusyBox",
    tools: &[],
    config: Config::Defconfig,
    args: &[
        "EXTRA_CFLAGS=-march=rv64gc -mabi=lp64d -Os",
        "EXTRA_LDFLAGS=-march=rv64gc -mabi=lp64d",
    ],
    artifact: "busybox",
};
