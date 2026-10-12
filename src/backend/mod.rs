pub mod asterinas;
mod builder;
pub mod busybox;
pub mod linux;
pub mod uboot;

pub const BACKENDS: &[&str] = &[
    "rustsbi",
    "asterinas",
    "prebuilt",
    "busybox",
    "linux",
    "uboot",
];

pub(crate) fn kbuild(name: &str) -> Option<&'static builder::Kbuild> {
    match name {
        "busybox" => Some(&busybox::PACKAGE),
        "linux" => Some(&linux::PACKAGE),
        "uboot" => Some(&uboot::PACKAGE),
        _ => None,
    }
}
