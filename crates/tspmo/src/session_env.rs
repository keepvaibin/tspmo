//! `tspmo session-env`: environment for every user session, run by systemd as a
//! user-environment generator (`/usr/lib/systemd/user-environment-generators/60-tspmo`).
//!
//! Keeps everyday work (GTK rendering, browser video decoding) on the iGPU so the NVIDIA GPU
//! stays asleep and can be powered off. Apps that ask for the dGPU still get it.
//! In Ultimate mode the iGPU drives nothing, so no overrides are emitted.

use tspmo_core::gpu::{self, Mode};
use tspmo_sys::{SysRoot, pci};

pub fn print(root: &SysRoot) {
    let dgpu = gpu::dgpu(root);
    if dgpu.as_ref().is_some_and(|g| g.vendor != pci::VENDOR_NVIDIA) {
        return; // AMD dGPUs don't have these problems
    }
    let has_nvidia = dgpu.is_some() || root.exists("/sys/module/nvidia");
    if gpu::status(root, None).mode == Mode::Ultimate {
        return;
    }
    if has_nvidia || gpu::switcher(root) != gpu::Switcher::None {
        // GTK 4's Vulkan renderer enumerates every GPU and wakes the dGPU at each app launch.
        println!("GSK_RENDERER=ngl");
    }
    // Hardware video decoding on the iGPU (Intel media engine), not the NVIDIA one.
    let igpu = pci::gpus(root)
        .into_iter()
        .find(|g| g.boot_vga && g.vendor == pci::VENDOR_INTEL);
    if let Some(igpu) = igpu {
        if root.exists("/usr/lib/dri/iHD_drv_video.so") {
            println!("LIBVA_DRIVER_NAME=iHD");
        }
        let render = format!("/dev/dri/by-path/pci-{}-render", igpu.address);
        if root.exists(&render) {
            println!("MOZ_DRM_DEVICE={render}");
        }
    }
}
