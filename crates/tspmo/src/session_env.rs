//! `tspmo session-env`: environment for every user session, run by systemd as a
//! user-environment generator (`/usr/lib/systemd/user-environment-generators/60-tspmo`).
//!
//! Keeps everyday work (GTK rendering, browser video decoding, EGL probing) on the iGPU so the
//! NVIDIA GPU stays asleep and can be powered off. Apps that ask for the dGPU still get it:
//! `tspmo-run` and `prime-run` (Vulkan, GLX), and KDE's "Run using dedicated graphics card".
//! In Ultimate mode the iGPU drives nothing, so no overrides are emitted.

use tspmo_core::gpu::{self, Mode};
use tspmo_sys::{SysRoot, pci};

pub fn print(root: &SysRoot) {
    for line in lines(root) {
        println!("{line}");
    }
}

/// `NAME=value` lines for the user environment.
pub fn lines(root: &SysRoot) -> Vec<String> {
    let mut out = Vec::new();
    let dgpu = gpu::dgpu(root);
    if dgpu.as_ref().is_some_and(|g| g.vendor != pci::VENDOR_NVIDIA) {
        return out; // AMD dGPUs don't have these problems
    }
    let has_nvidia = dgpu.is_some() || root.exists("/sys/module/nvidia");
    if gpu::status(root, None).mode == Mode::Ultimate {
        return out;
    }
    if has_nvidia || gpu::switcher(root) != gpu::Switcher::None {
        // GTK 4's Vulkan renderer enumerates every GPU and wakes the dGPU at each app launch.
        out.push("GSK_RENDERER=ngl".into());
        // GLVND's EGL enumerates every vendor and device: with NVIDIA's EGL installed, any app
        // that merely looks for a GPU (a browser's media decoder, a Qt or Electron app) opens
        // /dev/nvidia* and keeps the dGPU on, and Eco refuses to turn it off. Mesa only, as for
        // the compositor (`compositor.env`). The dGPU's own entries (`tspmo-run`, `prime-run`)
        // use Vulkan and GLX, which this does not touch.
        if let Some(mesa) = root
            .glob("/usr/share/glvnd/egl_vendor.d/*mesa*.json")
            .into_iter()
            .next()
        {
            out.push(format!("__EGL_VENDOR_LIBRARY_FILENAMES={mesa}"));
        }
    }
    // Hardware video decoding on the iGPU (Intel media engine), not the NVIDIA one.
    let igpu = pci::gpus(root)
        .into_iter()
        .find(|g| g.boot_vga && g.vendor == pci::VENDOR_INTEL);
    if let Some(igpu) = igpu {
        if root.exists("/usr/lib/dri/iHD_drv_video.so") {
            out.push("LIBVA_DRIVER_NAME=iHD".into());
        }
        let render = format!("/dev/dri/by-path/pci-{}-render", igpu.address);
        if root.exists(&render) {
            out.push(format!("MOZ_DRM_DEVICE={render}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An Intel + NVIDIA laptop with the ASUS GPU switch, in a temporary directory.
    fn laptop(tag: &str, mesa: bool) -> (std::path::PathBuf, SysRoot) {
        let dir = std::env::temp_dir().join(format!("tspmo-session-env-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut files = vec![
            ("sys/bus/pci/devices/0000:00:02.0/vendor", "0x8086\n"),
            ("sys/bus/pci/devices/0000:00:02.0/class", "0x030000\n"),
            ("sys/bus/pci/devices/0000:00:02.0/boot_vga", "1\n"),
            ("sys/bus/pci/devices/0000:01:00.0/vendor", "0x10de\n"),
            ("sys/bus/pci/devices/0000:01:00.0/class", "0x030200\n"),
            ("sys/bus/pci/devices/0000:01:00.0/boot_vga", "0\n"),
            ("usr/share/glvnd/egl_vendor.d/10_nvidia.json", "{}"),
        ];
        if mesa {
            files.push(("usr/share/glvnd/egl_vendor.d/50_mesa.json", "{}"));
        }
        for (f, c) in files {
            let p = dir.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, c).unwrap();
        }
        let root = SysRoot::new(&dir);
        (dir, root)
    }

    #[test]
    fn everyday_apps_get_mesa_egl_only() {
        let (dir, root) = laptop("mesa", true);
        let l = lines(&root);
        assert!(
            l.contains(&"__EGL_VENDOR_LIBRARY_FILENAMES=/usr/share/glvnd/egl_vendor.d/50_mesa.json".to_owned()),
            "{l:?}"
        );
        assert!(l.contains(&"GSK_RENDERER=ngl".to_owned()));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn without_mesa_egl_nothing_is_forced() {
        let (dir, root) = laptop("nomesa", false);
        let l = lines(&root);
        assert!(!l.iter().any(|x| x.starts_with("__EGL_VENDOR")), "{l:?}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
