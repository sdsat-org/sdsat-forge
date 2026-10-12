use super::builder::{Config, Kbuild};

pub(super) const PACKAGE: Kbuild = Kbuild {
    name: "U-Boot",
    tools: &[
        "bash",
        "flex",
        "bison",
        "bc",
        "perl",
        "python3",
        "pkg-config",
    ],
    config: Config::Olddefconfig,
    args: &["u-boot.bin"],
    artifact: "u-boot.bin",
};
