//! `NeedleEngine` — the C-ABI FFI seam to `libneedle.so`.
//!
//! Needle is a native 45M-parameter tool-calling engine shipped as a prebuilt
//! shared library (`libneedle.so` / `.dylib` / `.dll`), NOT a llama-server
//! GGUF model. The reference bindings live in the Python package
//! (`/opt/src/ai/model/needle/needle/__init__.py`, via `ctypes`); this module
//! encodes the same ABI once, in Rust, via `libloading`.
//!
//! The ABI is:
//!
//! ```text
//! int  needle_init(const char *system, const char *tools_json, const char *tool_index_path);
//! int  needle_complete(const char *text, int max_new_tokens, char *buffer, int buffer_len);
//! void needle_reset(void);
//! int  needle_load(const char *blob, uint64_t len);
//! ```
//!
//! - `needle_complete` writes a NUL-terminated JSON envelope into `buffer` and
//!   returns `>= 0` on success (negative on failure). The buffer must be large
//!   enough for the whole envelope; the Python package uses 65536 bytes.
//! - The engine keeps **global** state: one loaded weights blob and one active
//!   (system, tools_json, tool_index_path) binding. All FFI calls are therefore
//!   serialized through a process-wide lock, and the load/init/dispatch
//!   lifecycle mirrors the Python `Needle._bind()` exactly.
//!
//! The engine's availability is a **routing concern**, not a boot concern: if
//! the library cannot be resolved or loaded, `is_available()` returns `false`
//! and the `NeedlePreFilter` stage skips cleanly (falls through to the
//! classifier) — it never errors a request.

use std::ffi::CString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use common_core::sync::lock;
use libloading::{Library, Symbol};

use super::envelope::NeedleEnvelope;
use super::NeedleError;

/// The Needle engine version this wrapper was written against. The cache
/// directory key mirrors `needle/agent/fetch.py::ENGINE_VERSION`.
pub const ENGINE_VERSION: &str = "2.0.2";

/// Shared-library filename per platform (mirrors
/// `needle/agent/fetch.py::_lib_name`).
pub const LIB_NAME: &str = if cfg!(target_os = "macos") {
    "libneedle.dylib"
} else if cfg!(target_os = "windows") {
    "libneedle.dll"
} else {
    "libneedle.so"
};

/// Envelope buffer size, matching the Python package's default
/// `buffer_size=65536`.
pub const ENVELOPE_BUFFER_SIZE: usize = 65536;

/// The C ABI signature for `needle_init`.
type NeedleInit = unsafe extern "C" fn(*const i8, *const i8, *const i8) -> i32;
/// The C ABI signature for `needle_complete`.
type NeedleComplete = unsafe extern "C" fn(*const i8, i32, *mut i8, i32) -> i32;
/// The C ABI signature for `needle_reset`.
type NeedleReset = unsafe extern "C" fn();
/// The C ABI signature for `needle_load`.
type NeedleLoad = unsafe extern "C" fn(*const u8, u64) -> i32;

/// Serializes every FFI call: the C engine owns a single global weights blob
/// and binding, so concurrent callers would corrupt each other's state.
static ENGINE_LOCK: Mutex<()> = Mutex::new(());

/// Resolve the `libneedle` shared-library path, mirroring
/// `needle/__init__.py::_library_path`: `NEEDLE_LIB_PATH` override, then the
/// package (crate) directory, then the user cache directory. `None` when no
/// candidate exists on disk — the stage then skips cleanly.
pub fn resolve_library_path() -> Option<PathBuf> {
    if let Some(override_path) = std::env::var_os("NEEDLE_LIB_PATH") {
        let path = PathBuf::from(override_path);
        if path.exists() {
            return Some(path);
        }
        tracing::warn!(
            target: "router.needle.engine",
            path = %path.display(),
            "NEEDLE_LIB_PATH does not exist"
        );
    }

    // Package dir: the crate directory is the closest Rust analog of the
    // Python package directory (a vendored `libneedle.so` beside the code).
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let local = here.join(LIB_NAME);
    if local.exists() {
        return Some(local);
    }

    // User cache: `~/.cache/cactus-needle/<version>/libneedle.so`.
    if let Some(home) = dirs::home_dir() {
        let cached = home
            .join(".cache")
            .join("cactus-needle")
            .join(ENGINE_VERSION)
            .join(LIB_NAME);
        if cached.exists() {
            return Some(cached);
        }
    }

    None
}

/// A loaded `libneedle` handle with its four bound entry points.
///
/// Constructed by [`NeedleEngine::load`], which is fallible: a missing or
/// unloadable library yields a [`NeedleError`] and the caller should treat
/// the engine as unavailable. Once loaded the handle owns the library for its
/// lifetime.
///
/// # Safety
///
/// `libloading` binds raw function pointers from a shared library; `load` is
/// the single, audited place in the crate where the C ABI is dereferenced. All
/// entry-point invocations happen behind [`ENGINE_LOCK`] so concurrent callers
/// cannot corrupt the C engine's global weights/binding state.
pub struct NeedleEngine {
    _library: &'static Library,
    needle_init: Symbol<'static, NeedleInit>,
    needle_complete: Symbol<'static, NeedleComplete>,
    needle_reset: Symbol<'static, NeedleReset>,
    needle_load: Symbol<'static, NeedleLoad>,
}

// SAFETY: `Library` and `Symbol<'static, extern "C" fn>` are `Send + Sync`;
// every dereference is additionally serialized through `ENGINE_LOCK`.
unsafe impl Send for NeedleEngine {}
unsafe impl Sync for NeedleEngine {}

impl NeedleEngine {
    /// Load `libneedle` from `path` and bind the four entry points. Any
    /// missing symbol is a hard error — a partial ABI must never be used.
    ///
    /// # Safety
    ///
    /// Loading and binding raw function pointers is inherently unsafe; the
    /// returned engine dereferences them in `init`/`complete`/`reset`/`load`
    /// behind [`ENGINE_LOCK`]. Callers must not invoke the library through any
    /// other route (the Python process, a second handle) concurrently.
    pub fn load(path: &Path) -> Result<Self, NeedleError> {
        // SAFETY: `libloading::Library::new` is safe; `get` binds function
        // pointers that the library exports. We trust the library's ABI here
        // exactly as the Python `ctypes.CDLL` bindings do. `Box::leak` pins
        // the handle for `'static` so the bound `Symbol<'static, _>`s remain
        // valid for the process lifetime (the engine is loaded once at boot).
        let library: &'static Library = Box::leak(Box::new(
            unsafe { Library::new(path) }.map_err(|e| NeedleError::Library {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?,
        ));
        let needle_init = unsafe { library.get(b"needle_init\0") }.map_err(|e| {
            NeedleError::Library {
                path: path.display().to_string(),
                detail: format!("needle_init: {e}"),
            }
        })?;
        let needle_complete = unsafe { library.get(b"needle_complete\0") }.map_err(|e| {
            NeedleError::Library {
                path: path.display().to_string(),
                detail: format!("needle_complete: {e}"),
            }
        })?;
        let needle_reset = unsafe { library.get(b"needle_reset\0") }.map_err(|e| {
            NeedleError::Library {
                path: path.display().to_string(),
                detail: format!("needle_reset: {e}"),
            }
        })?;
        let needle_load = unsafe { library.get(b"needle_load\0") }.map_err(|e| {
            NeedleError::Library {
                path: path.display().to_string(),
                detail: format!("needle_load: {e}"),
            }
        })?;

        tracing::info!(
            target: "router.needle.engine",
            path = %path.display(),
            version = ENGINE_VERSION,
            "loaded libneedle"
        );
        Ok(Self {
            _library: library,
            needle_init,
            needle_complete,
            needle_reset,
            needle_load,
        })
    }

    /// Load tuned `.cact` weights into the engine (the `_weights` path in the
    /// Python bindings). `None` keeps whatever the engine already has loaded.
    ///
    /// The engine cannot unload weights once loaded, so a tuned blob becomes
    /// sticky for the process — mirror the Python agent's
    /// "construct agents that want the base model before any tuned one"
    /// constraint by loading weights exactly once per process.
    pub fn load_weights(&self, weights: Option<&Path>) -> Result<(), NeedleError> {
        let Some(weights) = weights else {
            return Ok(());
        };
        let blob = std::fs::read(weights).map_err(|e| NeedleError::Weights {
            path: weights.display().to_string(),
            detail: e.to_string(),
        })?;
        let _guard = lock(&ENGINE_LOCK);
        // SAFETY: `needle_load` reads `blob` (borrowed, alive for the call)
        // and copies what it needs internally; no pointer escapes the call.
        let rc = unsafe { (self.needle_load)(blob.as_ptr(), blob.len() as u64) };
        if rc != 0 {
            return Err(NeedleError::Weights {
                path: weights.display().to_string(),
                detail: format!(
                    "needle_load returned {rc} — the .cact archive is tied to the engine version"
                ),
            });
        }
        Ok(())
    }

    /// (Re)bind the engine to a `(system, tools_json, tool_index_path)` triple,
    /// mirroring `Needle._bind`. Called before every `complete` — the C side
    /// re-grammars from `tools_json` and re-indexes from `tool_index_path`.
    fn bind(
        &self,
        system: &str,
        tools_json: &str,
        tool_index_path: Option<&str>,
    ) -> Result<(), NeedleError> {
        let system = cstring(system)?;
        let tools_json = cstring(tools_json)?;
        let index = match tool_index_path {
            Some(p) => Some(cstring(p)?),
            None => None,
        };
        let index_ptr = index.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());
        // SAFETY: all three pointers reference live `CString`s valid for the
        // duration of the call; the engine copies them as needed.
        let rc = unsafe { (self.needle_init)(system.as_ptr(), tools_json.as_ptr(), index_ptr) };
        if rc < 0 {
            return Err(NeedleError::Init {
                detail: format!("needle_init returned {rc}"),
            });
        }
        Ok(())
    }

    /// Run one completion and parse the envelope from the output buffer.
    ///
    /// The engine writes a NUL-terminated JSON envelope into the caller-owned
    /// buffer and returns `>= 0` on success. `max_new_tokens` bounds
    /// generation; the returned envelope's JSON is grammar-constrained, so a
    /// parse failure here is an engine bug surfaced as
    /// [`NeedleError::MalformedEnvelope`].
    fn complete(
        &self,
        text: &str,
        max_new_tokens: i32,
        buffer: &mut [u8],
    ) -> Result<NeedleEnvelope, NeedleError> {
        let text = cstring(text)?;
        // SAFETY: `text` references a live `CString`; `buffer` is a caller-
        // owned mutable slice of at least the buffer length we pass.
        let rc = unsafe {
            (self.needle_complete)(
                text.as_ptr(),
                max_new_tokens,
                buffer.as_mut_ptr().cast(),
                buffer.len() as i32,
            )
        };
        if rc < 0 {
            return Err(NeedleError::Complete {
                detail: format!("needle_complete returned {rc}"),
            });
        }
        // The buffer is NUL-terminated by the engine; read up to the first
        // NUL. A fully-set buffer with no terminator is an engine bug.
        let end = buffer
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| NeedleError::MalformedEnvelope {
                detail: "envelope buffer not NUL-terminated".into(),
            })?;
        let raw = std::str::from_utf8(&buffer[..end]).map_err(|e| {
            NeedleError::MalformedEnvelope {
                detail: format!("envelope not valid UTF-8: {e}"),
            }
        })?;
        NeedleEnvelope::parse(raw)
    }

    /// Reset the engine's session state (context cache), mirroring
    /// `Needle.reset`.
    pub fn reset(&self) {
        let _guard = lock(&ENGINE_LOCK);
        // SAFETY: `needle_reset` takes no arguments and operates on internal
        // state only.
        unsafe { (self.needle_reset)() };
    }
}

/// Build a `CString` from a Rust string. Needle text and schemas are UTF-8
/// JSON/text; embedded NULs are rejected rather than silently truncated.
fn cstring(value: &str) -> Result<CString, NeedleError> {
    CString::new(value).map_err(|e| NeedleError::Input {
        detail: format!("embedded NUL in engine input: {e}"),
    })
}

// ── Process-wide availability flag ─────────────────────────────────────────

/// Set once, after a successful [`NeedleEngine::load`]. The stage uses it to
/// skip cleanly when the engine is unavailable (never error the request).
static AVAILABLE: AtomicBool = AtomicBool::new(false);

/// Whether `libneedle` is loaded and usable in this process.
pub fn engine_available() -> bool {
    AVAILABLE.load(Ordering::SeqCst)
}

/// Mark the process-wide engine available. Called once, after a successful
/// [`NeedleEngine::load`].
pub(crate) fn mark_available() {
    AVAILABLE.store(true, Ordering::SeqCst);
}

// ── Production `NeedleBackend` implementation ──────────────────────────────

/// The production `NeedleBackend` over the FFI.
///
/// Holds the loaded engine plus the configured system prompt and tool-index
/// path; `complete` re-binds the engine when the caller's `tools_json`
/// differs from the last bound value (mirroring `Needle._bind`'s `_active`
/// idempotency).
pub struct NativeNeedleEngine {
    engine: NeedleEngine,
    system: String,
    tool_index_path: Option<String>,
    /// Last-bound (system, tools_json) — `needle_init` is skipped when the
    /// caller asks for the same schema (cheap idempotency, as in Python).
    bound: Mutex<Option<(String, String)>>,
}

impl NativeNeedleEngine {
    /// Load the engine and construct the backend. `weights` are loaded into
    /// the engine exactly once (sticky for the process, matching the Python
    /// constraint).
    pub fn load(
        path: &Path,
        system: impl Into<String>,
        tool_index_path: Option<String>,
        weights: Option<&Path>,
    ) -> Result<Self, NeedleError> {
        let engine = NeedleEngine::load(path)?;
        engine.load_weights(weights)?;
        mark_available();
        Ok(Self {
            engine,
            system: system.into(),
            tool_index_path,
            bound: Mutex::new(None),
        })
    }
}

impl super::backend::NeedleBackend for NativeNeedleEngine {
    fn complete(
        &self,
        text: &str,
        tools_json: &str,
        max_new_tokens: i32,
    ) -> Result<NeedleEnvelope, NeedleError> {
        let _guard = lock(&ENGINE_LOCK);
        {
            let mut bound = lock(&self.bound);
            let needs_bind = match bound.as_ref() {
                Some((system, tools)) => system != &self.system || tools != tools_json,
                None => true,
            };
            if needs_bind {
                self.engine
                    .bind(&self.system, tools_json, self.tool_index_path.as_deref())?;
                *bound = Some((self.system.clone(), tools_json.to_string()));
            }
        }
        let mut buffer = vec![0u8; ENVELOPE_BUFFER_SIZE];
        self.engine.complete(text, max_new_tokens, &mut buffer)
    }

    fn is_available(&self) -> bool {
        true
    }

    fn reset(&self) {
        self.engine.reset();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lib_name_is_platform_specific() {
        assert!(!LIB_NAME.is_empty());
        assert!(
            LIB_NAME.ends_with(".so")
                || LIB_NAME.ends_with(".dylib")
                || LIB_NAME.ends_with(".dll")
        );
    }

    #[test]
    fn bogus_needle_lib_path_env_is_ignored() {
        // A bogus override must never be used. The resolver falls back to the
        // package/cache dirs; when a real lib is present (e.g. the crate-dir
        // symlink on a dev box) it resolves there — never to the bogus path.
        let bogus = "/nonexistent/libneedle.so";
        unsafe { std::env::set_var("NEEDLE_LIB_PATH", bogus) };
        let resolved = resolve_library_path();
        unsafe { std::env::remove_var("NEEDLE_LIB_PATH") };
        assert_ne!(
            resolved.as_deref(),
            Some(Path::new(bogus)),
            "bogus NEEDLE_LIB_PATH override must be ignored, got {resolved:?}"
        );
    }

    #[test]
    fn load_missing_library_is_an_error() {
        let result = NeedleEngine::load(Path::new("/nonexistent/libneedle.so"));
        assert!(
            matches!(result, Err(NeedleError::Library { .. })),
            "missing library must surface a Library error"
        );
    }

    #[test]
    fn cstring_rejects_embedded_nul() {
        let err = cstring("has\0nul").expect_err("nul");
        assert!(matches!(err, NeedleError::Input { .. }));
    }
}