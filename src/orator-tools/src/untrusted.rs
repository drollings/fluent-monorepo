//! Compatibility re-export: the `<data>` envelope lives in
//! `orator-core::untrusted` (single home); this module re-exports it
//! so existing tool-loop imports keep working.

pub use orator_core::untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};

#[cfg(test)]
#[path = "../tests/untrusted.rs"]
mod tests;
