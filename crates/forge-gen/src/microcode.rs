//! CPU microcode at boot. The `microcode` package ships early images (`/usr/lib/microcode/early-<vendor>.cpio`); forge
//! keeps the one for this CPU in the boot directory as `microcode.img`, and every Limine entry loads it before
//! anything else so the kernel can apply it before it mounts a thing.

use std::path::{Path, PathBuf};

/// Where the package puts its images, relative to a system root.
const IMAGE_DIR: &str = "usr/lib/microcode";

/// `amd` or `intel`, from the CPU this machine has (`FORGE_CPU_VENDOR` overrides it, for tests); None when unknown.
pub fn cpu_vendor() -> Option<&'static str> {
    let text = match std::env::var("FORGE_CPU_VENDOR") {
        Ok(v) => format!("vendor_id: {v}"),
        Err(_) => std::fs::read_to_string("/proc/cpuinfo").ok()?,
    };
    let line = text.lines().find(|l| l.starts_with("vendor_id"))?;
    if line.contains("AuthenticAMD") {
        Some("amd")
    } else if line.contains("GenuineIntel") {
        Some("intel")
    } else {
        None
    }
}

/// The image for this CPU's vendor under `root`; None for a CPU of another vendor (nothing to load).
/// (The path only; the file may not be installed yet.)
pub fn image_path(root: &Path) -> Option<PathBuf> {
    cpu_vendor().map(|v| root.join(IMAGE_DIR).join(format!("early-{v}.cpio")))
}

/// Copy the image into `boot` as `microcode.img` when the package is installed; returns whether there is one.
pub fn install_image(root: &Path, boot: &Path) -> std::io::Result<bool> {
    let Some(src) = image_path(root).filter(|s| s.is_file()) else {
        return Ok(false);
    };
    let dest = boot.join(crate::MICROCODE_IMAGE);
    let tmp = boot.join(format!(".{}.new", crate::MICROCODE_IMAGE));
    std::fs::copy(&src, &tmp)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_image_follows_the_cpu_vendor() {
        // Only this test sets the variable, and only `cpu_vendor` reads it.
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "AuthenticAMD") };
        assert!(image_path(Path::new("/r")).unwrap().ends_with("usr/lib/microcode/early-amd.cpio"));
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "GenuineIntel") };
        assert!(image_path(Path::new("/r")).unwrap().ends_with("early-intel.cpio"));
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "SomethingElse") };
        assert_eq!(image_path(Path::new("/r")), None, "another vendor has nothing to load");
        unsafe { std::env::remove_var("FORGE_CPU_VENDOR") };
    }
}
