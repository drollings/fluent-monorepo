//! Terminal typeface: Noto Sans over the scaffolding's chain policy.
//!
//! This module names the concrete bundle — `NotoSans` (proportional) plus
//! `NotoSansMono` (monospace) from the crate's `fonts/` directory — and
//! delegates every chain mechanic to `fluent_egui :: fonts`. Nothing here
//! parses bytes or orders chains; the scaffolding owns that policy, the
//! examples own the `include_bytes!` call (the crate ships no embedded blob
//! in `src/`), and this module is the one place that says which bundle the
//! terminal paints with.

// Display-capable proportional primary (Latin, Greek, Cyrillic, currency
// and arrow symbols); CJK ideographs are not in this file.
pub const NOTO_SANS_NAME: &str = "NotoSans";
// Monospace primary for tables, tickets, and aligned numbers.
pub const NOTO_SANS_MONO_NAME: &str = "NotoSansMono";

/// Queue the terminal bundle on a live context: Noto Sans first for
/// proportional text, Noto Sans Mono first for monospace, with whatever
/// was installed before staying behind as the fallback.
///
/// The mono bundle also rides as the proportional fallback ahead of the
/// defaults: Noto Sans itself lacks the arrows and geometric shapes the
/// terminal paints (`→` replay ranges, `◀`/`▶` month and replay buttons)
/// while the mono file carries them. Both legs ship in the mono insert
/// because `Context :: add_font` skips a name it already holds — a fallback
/// leg queued after the primary under the same name would never take
/// effect, so the mono insert is queued before the sans primary.
///
/// Call once from the eframe setup closure with the crate's `fonts/`
/// bytes; validation failures refuse before anything is queued.
pub fn install_noto_sans(
    ctx: &egui::Context,
    sans: &'static [u8],
    mono: &'static [u8],
) -> Result<(), fluent_egui::EguiError> {
    let mono = fluent_egui::primary_with_fallback_insert(
        NOTO_SANS_MONO_NAME,
        mono,
        egui::FontFamily::Monospace,
        egui::FontFamily::Proportional,
    )?;
    let sans = fluent_egui::proportional_font_insert(NOTO_SANS_NAME, sans)?;
    ctx.add_font(mono);
    ctx.add_font(sans);
    Ok(())
}

#[cfg(test)]
#[path = "../tests/fonts.rs"]
mod tests;
