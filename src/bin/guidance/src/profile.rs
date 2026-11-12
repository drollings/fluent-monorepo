//! Opt-in phase timing: `GUIDANCE_PROFILE=1` prints `[profile] name: N ms`
//! lines to stderr. Off by default so normal output and timing stay untouched.

use std::time::Instant;

/// Whether phase timing is requested for this process.
#[must_use]
/// Whether phase timing is requested for this process.
pub fn enabled() -> bool {
    std::env::var_os("GUIDANCE_PROFILE").is_some_and(|value| value != "0")
}

/// Print one accumulated phase duration when profiling is enabled.
pub fn report(name: &str, elapsed_ms: u128) {
    if enabled() {
        eprintln!("[profile] {name}: {elapsed_ms} ms");
    }
}

/// Print one cardinality when profiling is enabled.
pub fn count(name: &str, value: usize) {
    if enabled() {
        eprintln!("[profile] {name}: {value}");
    }
}

/// RAII phase timer: prints on drop when profiling is enabled.
pub struct Phase {
    name: &'static str,
    start: Instant,
    enabled: bool,
}

impl Phase {
    /// Start timing `name`.
    #[must_use]
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            start: Instant::now(),
            enabled: enabled(),
        }
    }

    /// Elapsed milliseconds since the phase started.
    #[must_use]
    pub fn elapsed_ms(&self) -> u128 {
        self.start.elapsed().as_millis()
    }
}

impl Drop for Phase {
    fn drop(&mut self) {
        if self.enabled {
            eprintln!("[profile] {}: {} ms", self.name, self.elapsed_ms());
        }
    }
}
