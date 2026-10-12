use super::builder::{Config, Kbuild};

pub(super) const PACKAGE: Kbuild = Kbuild {
    name: "Linux",
    tools: &["bash", "flex", "bison", "bc", "perl"],
    config: Config::Olddefconfig,
    args: &["Image"],
    artifact: "arch/riscv/boot/Image",
};
