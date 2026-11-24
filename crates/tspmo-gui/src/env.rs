//! Keep the NVIDIA GPU asleep: this program draws on the CPU, and if anything in the graphics
//! stack looks for a GPU it must find Mesa only.

const MESA_EGL: &str = "/usr/share/glvnd/egl_vendor.d/50_mesa.json";

/// Must run first in `main`, before any thread exists.
#[allow(unsafe_code)]
pub fn force_mesa() {
    // SAFETY: called at the top of `main` while the process is still single-threaded.
    unsafe {
        if std::path::Path::new(MESA_EGL).exists() {
            std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", MESA_EGL);
        }
        std::env::set_var("DRI_PRIME", "0");
        std::env::set_var("__GLX_VENDOR_LIBRARY_NAME", "mesa");
        std::env::set_var("__NV_PRIME_RENDER_OFFLOAD", "0");
    }
}
