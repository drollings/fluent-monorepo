//! Sidecar instance-management policy — the `sidecar` section of
//! `RouterConfig` — plus the residency-eviction vocabulary.

use serde::{Deserialize, Serialize};

/// Residency-eviction policy vocabulary. Only the sidecar residency loop
/// interprets it today; the type lives here so the `sidecar` section and its
/// future consumers share one spelling.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum EvictionPolicy {
    #[default]
    Lru,
    Ttl,
    Hybrid,
}

/// Sidecar instance-management policy.
///
/// The sidecar task is the external VRAM-policy owner the fork's docs
/// describe: it boot-reconciles configured instance profiles against
/// `GET /instances`, polls `/memory`, and evicts least-recently-used unpinned
/// instances when free device VRAM drops below the watermark. It only ever
/// allocates or frees KV + compute buffers - the shared weights stay loaded in
/// `llama-server`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarConfig {
    /// How often the residency loop polls `/memory`, in seconds.
    #[serde(default = "default_sidecar_poll_interval_s")]
    pub poll_interval_s: u64,
    /// Free-VRAM threshold (bytes) below which the residency loop evicts.
    #[serde(default = "default_sidecar_watermark")]
    pub vram_low_watermark_bytes: u64,
    /// Max unpinned instances evicted per low-VRAM pass.
    #[serde(default = "default_sidecar_evict_batch")]
    pub evict_batch: usize,
    /// Device VRAM ceiling (bytes). `None` disables residency eviction (the
    /// loop still polls and logs) because free VRAM cannot be computed.
    #[serde(default)]
    pub vram_total_bytes: Option<u64>,
    /// Free-VRAM floor (bytes) that must remain unallocated: the effective
    /// allocation limit is `device_total - minimum_remaining_vram`. When
    /// `vram_total_bytes` is `None`, the device total is detected at boot
    /// (ROCm `mem_info_vram_total`); `minimum_remaining_vram` then alone
    /// enables the residency eviction budget.
    #[serde(default)]
    pub minimum_remaining_vram: Option<u64>,
    /// Slot-save directory the fork writes KV snapshots under
    /// (`<slot_save_path>/<model_key>/<instance>/`). Feeds snapshot-path derivation.
    #[serde(default)]
    pub slot_save_path: Option<String>,
    /// Resume snapshots older than this many seconds of context idle are
    /// dropped and their contexts' `resume` flag cleared: the router's signal
    /// that an evicted workload is done and need not be restorable. `None`
    /// keeps resume snapshots until explicitly disabled. The flag also feeds
    /// the `-resume` snapshot naming the router uses on eviction.
    #[serde(default)]
    pub resume_ttl_s: Option<u64>,
    /// Non-resume KV snapshots (per-turn `<session>-<seq>-<hash>` files, rigor
    /// blue snapshots) older than this many seconds (fork `mtime`) are deleted
    /// from the fork by the residency pass. `None` disables the age sweep —
    /// snapshots then accumulate until the byte budget (or an operator)
    /// removes them. Resume snapshots (`<instance>-resume`) are never touched
    /// by this sweep; they follow `resume_ttl_s` above.
    #[serde(default)]
    pub snapshot_ttl_s: Option<u64>,
    /// Byte budget for non-resume KV snapshots per model: when their summed
    /// fork-reported sizes exceed this, the residency pass deletes oldest
    /// (by fork `mtime`) first until under budget. `None` disables the budget
    /// sweep. Combines with `snapshot_ttl_s` (the age sweep runs first).
    /// Resume snapshots are excluded from both the sum and the sweep.
    #[serde(default)]
    pub snapshot_budget_bytes: Option<u64>,
    /// Env var naming the management API key sent as `Authorization: Bearer`.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Post-boot liveness poll: how often the supervision task probes a running
    /// server's `/health` (seconds). A server that stays alive but stops
    /// answering `liveness_failures_before_restart` consecutive probes is
    /// killed and restarted.
    #[serde(default = "default_sidecar_liveness_poll_s")]
    pub liveness_poll_interval_s: u64,
    /// Consecutive failed `/health` probes before a hung server is killed and
    /// restarted.
    #[serde(default = "default_sidecar_liveness_failures")]
    pub liveness_failures_before_restart: u32,
    /// Consecutive crashes (spawn failures or boot-time child exits) after
    /// which the supervisor stops restarting a model's `llama-server` and
    /// marks it **failed** (containment, per the fluent-concurrency
    /// supervision contract — no endless crash loop). `ensure_running` then
    /// returns a terminal error until the router restarts or the model is
    /// unloaded, at which point a fresh (bounded) load attempt is allowed.
    /// The count resets the moment a server answers `/health`, so a crash
    /// after a healthy period is a fresh failure. `0` disables the limit
    /// (unbounded restart with rising backoff).
    #[serde(default = "default_sidecar_max_restarts")]
    pub max_restarts: u32,
    /// The onnx fleet's working-set budget (bytes): when Σ resident bytes of
    /// loaded onnx sessions exceeds it, the onnx residency loop releases the
    /// LRU-largest `Unloadable` sessions. `None` (the default) → idle-only
    /// eviction — CPU RAM is cheap and the parity target is idle unload, not
    /// a tight budget.
    #[serde(default)]
    pub onnx_working_set_budget_bytes: Option<u64>,
    /// Persisted fleet map (`{model: {port, pid}}` for every running server)
    /// the supervisor writes at boot and reads back on the next boot so an
    /// orphaned `llama-server` (router killed without graceful shutdown) is
    /// adopted instead of duplicated. `None` (the default) disables
    /// persistence — adoption falls back to the `/proc` scan alone.
    #[serde(default)]
    pub server_state_path: Option<String>,
}

impl Default for SidecarConfig {
    fn default() -> Self {
        Self {
            poll_interval_s: default_sidecar_poll_interval_s(),
            vram_low_watermark_bytes: default_sidecar_watermark(),
            evict_batch: default_sidecar_evict_batch(),
            vram_total_bytes: None,
            minimum_remaining_vram: None,
            slot_save_path: None,
            resume_ttl_s: None,
            snapshot_ttl_s: None,
            snapshot_budget_bytes: None,
            api_key_env: None,
            liveness_poll_interval_s: default_sidecar_liveness_poll_s(),
            liveness_failures_before_restart: default_sidecar_liveness_failures(),
            max_restarts: default_sidecar_max_restarts(),
            onnx_working_set_budget_bytes: None,
            server_state_path: None,
        }
    }
}

const fn default_sidecar_poll_interval_s() -> u64 {
    5
}

const fn default_sidecar_watermark() -> u64 {
    1073741824
}

const fn default_sidecar_evict_batch() -> usize {
    1
}

const fn default_sidecar_liveness_poll_s() -> u64 {
    30
}

const fn default_sidecar_liveness_failures() -> u32 {
    3
}

const fn default_sidecar_max_restarts() -> u32 {
    5
}

/// Detect the device VRAM total (bytes) from the ROCm sysfs interface. Returns
/// the first non-zero `mem_info_vram_total` found under `/sys/class/drm`. Used
/// when `sidecar.vram_total_bytes` is unset so a `minimum_remaining_vram`
/// budget alone can drive the residency loop. `None` when the interface is
/// absent (non-ROCm hosts).
pub fn detect_device_vram_total() -> Option<u64> {
    let entries = fluent_wvr::capability::capability_aware_fs::read_dir("/sys/class/drm").ok()?;
    for entry in entries.flatten() {
        let path = entry.path().join("device/mem_info_vram_total");
        let text = fluent_wvr::capability::capability_aware_fs::read_to_string(path).ok()?;
        let total = text.trim().parse::<u64>().ok()?;
        if total > 0 {
            return Some(total);
        }
    }
    None
}

impl SidecarConfig {
    /// The device VRAM total: the explicit `vram_total_bytes` ceiling first,
    /// else the ROCm sysfs detection. `None` when neither is available.
    pub fn device_total_bytes(&self) -> Option<u64> {
        self.vram_total_bytes.or_else(detect_device_vram_total)
    }

    /// The effective VRAM allocation budget: `device_total - minimum_remaining
    /// _vram`. `None` when no device total is available, or when neither a
    /// ceiling nor a minimum-remaining floor is configured (eviction off).
    /// This is the pre-admission hard gate: a cold load frees room against
    /// this before the spawn, sized with its incoming KV window.
    pub fn allocation_limit(&self) -> Option<u64> {
        let total = self.device_total_bytes()?;
        let min_remaining = self.minimum_remaining_vram.unwrap_or(0);
        Some(total.saturating_sub(min_remaining))
    }

    /// The reactive residency budget: `device_total - vram_low_watermark_bytes`.
    /// `None` when no device total is available. This is the periodic loop's
    /// floor only — it never gates pre-admission, so a load that fits the
    /// allocation budget stays quiet even past the watermark-implied usage.
    pub fn reactive_limit(&self) -> Option<u64> {
        let total = self.device_total_bytes()?;
        Some(total.saturating_sub(self.vram_low_watermark_bytes))
    }
}
