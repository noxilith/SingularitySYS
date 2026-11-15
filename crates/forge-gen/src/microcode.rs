//! CPU microcode at boot. The `microcode` package ships early images (`/usr/lib/microcode/early*.cpio`); forge keeps
//! the right one in the boot directory as `microcode.img`, and every Limine entry loads it before anything else so the
//! kernel can apply it before it mounts a thing.

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
    if line.contains("AuthenticAMD") || line.ends_with("amd") {
        Some("amd")
    } else if line.contains("GenuineIntel") || line.ends_with("intel") {
        Some("intel")
    } else {
        None
    }
}

/// The image to use under `root`: the one for this CPU's vendor, or the one with both when the vendor is unknown.
/// (The path only; the file may not be installed yet.)
pub fn image_path(root: &Path) -> PathBuf {
    let name = match cpu_vendor() {
        Some(v) => format!("early-{v}.cpio"),
        None => "early.cpio".to_string(),
    };
    root.join(IMAGE_DIR).join(name)
}

/// Copy the image into `boot` as `microcode.img` when the package is installed; returns whether there is one.
pub fn install_image(root: &Path, boot: &Path) -> std::io::Result<bool> {
    let src = image_path(root);
    if !src.is_file() {
        return Ok(false);
    }
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
        // SAFETY of env use: tests in this module only; the variable is read by `cpu_vendor` alone.
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "AuthenticAMD") };
        assert!(image_path(Path::new("/r")).ends_with("usr/lib/microcode/early-amd.cpio"));
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "GenuineIntel") };
        assert!(image_path(Path::new("/r")).ends_with("early-intel.cpio"));
        unsafe { std::env::set_var("FORGE_CPU_VENDOR", "SomethingElse") };
        assert!(image_path(Path::new("/r")).ends_with("early.cpio"), "an unknown CPU gets the image with both");
        unsafe { std::env::remove_var("FORGE_CPU_VENDOR") };
    }
}
