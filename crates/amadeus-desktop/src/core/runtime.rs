#[cfg(target_os = "linux")]
pub fn configure_linux_runtime() {
    use std::env;

    let session_type = env::var("XDG_SESSION_TYPE").unwrap_or_default();
    let has_x11_display = env::var_os("DISPLAY").is_some();

    if session_type.eq_ignore_ascii_case("wayland") && has_x11_display {
        force_x11_backend("GDK_BACKEND");
        force_x11_backend("WINIT_UNIX_BACKEND");
        force_x11_backend("GLFW_PLATFORM");

        if env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
            unsafe {
                env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
            }
        }
    }
}

#[cfg(target_os = "linux")]
fn force_x11_backend(name: &str) {
    use std::env;

    let should_override = env::var(name)
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            normalized.is_empty()
                || normalized == "wayland"
                || normalized == "any"
                || (normalized.contains("wayland") && !normalized.contains("x11"))
        })
        .unwrap_or(true);

    if should_override {
        unsafe {
            env::set_var(name, "x11");
        }
    }
}

#[cfg(not(target_os = "linux"))]
pub fn configure_linux_runtime() {}
