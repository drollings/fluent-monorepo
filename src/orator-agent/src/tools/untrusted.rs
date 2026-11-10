//! Compatibility re-export: the `<data>` envelope lives in
//! `orator::untrusted` (single home); this module re-exports it
//! so existing tool-loop imports keep working.

pub use orator::untrusted::{wrap_tool_result, wrap_untrusted, TRANSCRIPT_AS_DATA};

#[cfg(test)]
#[path = "../../tests/tools_untrusted.rs"]
mod tests;
