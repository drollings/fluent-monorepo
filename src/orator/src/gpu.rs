//! GPU device presence: one probe for every backend's opt-in offload.
//!
//! The `gpu` leaves stay `false` unless the operator opts in. When set,
//! each backend checks this probe before attempting offload: no device
//! means a named error, never a silent CPU fallback. Presence is a
//! necessary condition, not a guarantee — the backend's own stack can
//! still fail loudly at load, which is the actionable outcome.

/// Whether a GPU device node is visible to this process.
///
/// Linux checks the NVIDIA nodes and the DRM render nodes; macOS always
/// reports present (Metal is integral); other platforms report absent
/// (out of scope, fail closed toward CPU).
#[must_use]
/// GPU device presence: one probe for every backend's opt-in offload.
/// 
/// The `gpu` leaves stay `false` unless the operator opts in. When set,
/// each backend checks this probe before attempting offload: no device
/// means a named error, never a silent CPU fallback. Presence is a
/// necessary condition, not a guarantee — the backend's own stack can
/// still fail loudly at load, which is the actionable outcome.
/// Whether a GPU device node is visible to this process.
/// 
/// Linux checks the NVIDIA nodes and the DRM render nodes; macOS always
/// reports present (Metal is integral); other platforms report absent
/// (out of scope, fail closed toward CPU).
pub fn device_present() -> bool {
    #[cfg(target_os = "macos")]
    {
        true
    }
    #[cfg(target_os = "windows")]
    {
        false
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        linux_device_present()
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn linux_device_present() -> bool {
    std::path::Path::new("/dev/nvidia0").exists()
        || std::path::Path::new("/dev/nvidiactl").exists()
        || has_render_node()
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn has_render_node() -> bool {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with("renderD"))
    })
}

/// Whether an NVIDIA device node is visible to this process.
///
/// `device_present` also fires on AMD render nodes (ROCm); several
/// backends only speak CUDA, so they need the narrower gate: requesting
/// the CUDA execution provider without NVIDIA hardware can never
/// succeed, and failing actionably beats a doomed probe.
#[must_use]
/// Whether an NVIDIA device node is visible to this process.
/// 
/// `device_present` also fires on AMD render nodes (ROCm); several
/// backends only speak CUDA, so they need the narrower gate: requesting
/// the CUDA execution provider without NVIDIA hardware can never
/// succeed, and failing actionably beats a doomed probe.
pub fn nvidia_present() -> bool {
    #[cfg(target_os = "macos")]
    {
        false
    }
    #[cfg(target_os = "windows")]
    {
        false
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::path::Path::new("/dev/nvidia0").exists()
            || std::path::Path::new("/dev/nvidiactl").exists()
    }
}

/// Require a visible GPU device for `kind` (`whisper`, `kokoro`,
/// `llama-server`): `Ok` when present, a named `Err` otherwise. The
/// caller maps this into its own error taxonomy.
pub fn require_gpu(kind: &str) -> Result<(), String> {
    if device_present() {
        Ok(())
    } else {
        Err(format!(
            "GPU offload requested for {kind} but no GPU device was found (set the backend's `gpu` leaf to false for CPU)"
        ))
    }
}

#[cfg(test)]
#[path = "../tests/gpu.rs"]
mod tests;