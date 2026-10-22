//! Bundled-font installation over egui's fallback chain.
//!
//! The bundled defaults (`Hack` + `Ubuntu-Light`) cover Latin and Cyrillic
//! only, so any wider script renders as tofu unless the composition root
//! installs more. `TextStyle` cannot fix that: it resolves to a size and a
//! family, never to a typeface. This module is the one policy for fixing it
//! from the scaffolding side — pure builders that prepend a bundled font to
//! the fallback chain plus thin `Context` wrappers that queue the install.
//!
//! Headless-first: every builder is `FontDefinitions`-in/`FontDefinitions`-
//! out with no `Context`, so suites assert the chain without opening a
//! window. Byte parsing stays where it belongs (at first use); the builders
//! only refuse what is unusable on its face — a blank name, empty bytes, or
//! an empty family list — and never substitute a font.
//!
//! The bytes themselves live with the caller (the terminal crate ships its
//! `fonts/` directory and passes the slice in), so this crate embeds no
//! binary blob and gains no new dependency. Previously installed fonts stay
//! behind the bundle as fallbacks: a `Highest` insert never removes the
//! defaults, it only outranks them, so glyphs the bundle lacks still resolve
//! exactly as before instead of turning into new tofu.

use crate::error::EguiError;

// `FontInsert` / `InsertFontFamily` / `FontPriority` are not re-exported at
// the `egui` root in the pinned 0.36; they ride along through the re-exported
// `epaint` crate instead.
use egui::epaint::text::{FontInsert, FontPriority, InsertFontFamily};

/// Refuse a blank font name before it can enter a fallback chain.
fn checked_name(name: &str) -> Result<String, EguiError> {
    if name.trim().is_empty() {
        return Err(EguiError::InvalidParam {
            what: "font name".to_owned(),
            reason: "must not be blank".to_owned(),
        });
    }
    Ok(name.to_owned())
}

/// Refuse an empty byte slice before it can enter a fallback chain.
///
/// Parsing stays at first use; this only refuses what is unusable on its
/// face.
fn checked_bytes(bytes: &'static [u8]) -> Result<&'static [u8], EguiError> {
    if bytes.is_empty() {
        return Err(EguiError::InvalidParam {
            what: "font bytes".to_owned(),
            reason: "must not be empty".to_owned(),
        });
    }
    Ok(bytes)
}

/// Build one fallback-chain insert from validated parts.
///
/// The families list must name at least one family; an empty insert would
/// install bytes no text lookup can ever reach.
pub fn font_insert(
    name: &str,
    bytes: &'static [u8],
    families: Vec<InsertFontFamily>,
) -> Result<FontInsert, EguiError> {
    let name = checked_name(name)?;
    let bytes = checked_bytes(bytes)?;
    if families.is_empty() {
        return Err(EguiError::InvalidParam {
            what: "font families".to_owned(),
            reason: "at least one family is required".to_owned(),
        });
    }
    Ok(FontInsert::new(
        &name,
        egui::FontData::from_static(bytes),
        families,
    ))
}

/// Highest-priority insert for one family: the named font is consulted
/// before every previously installed font of that family.
fn highest_for(family: egui::FontFamily) -> InsertFontFamily {
    InsertFontFamily {
        family,
        priority: FontPriority::Highest,
    }
}

/// One insert that makes the named font the primary for the proportional
/// family, leaving monospace untouched.
pub fn proportional_font_insert(name: &str, bytes: &'static [u8]) -> Result<FontInsert, EguiError> {
    font_insert(
        name,
        bytes,
        vec![highest_for(egui::FontFamily::Proportional)],
    )
}

/// One insert that makes the named font the primary for the monospace
/// family, leaving proportional untouched.
pub fn monospace_font_insert(name: &str, bytes: &'static [u8]) -> Result<FontInsert, EguiError> {
    font_insert(name, bytes, vec![highest_for(egui::FontFamily::Monospace)])
}

/// One insert that makes the named font the primary for both families.
///
/// Prefer the per-family inserts when proportional and monospace have
/// different primaries (the terminal does); use this only when one bundle
/// genuinely serves both.
pub fn bundled_font_insert(name: &str, bytes: &'static [u8]) -> Result<FontInsert, EguiError> {
    font_insert(
        name,
        bytes,
        vec![
            highest_for(egui::FontFamily::Proportional),
            highest_for(egui::FontFamily::Monospace),
        ],
    )
}

/// One insert with split priority: `Highest` for `primary`, `Lowest`
/// fallback for `fallback`.
///
/// The terminal uses this for its mono bundle — the monospace primary that
/// also backstops proportional text (arrows, geometric shapes) ahead of the
/// previously installed defaults. Building both legs into the first insert
/// matters: `Context :: add_font` skips a name it already holds, so a
/// fallback leg queued after the primary under the same name would never
/// take effect.
pub fn primary_with_fallback_insert(
    name: &str,
    bytes: &'static [u8],
    primary: egui::FontFamily,
    fallback: egui::FontFamily,
) -> Result<FontInsert, EguiError> {
    if primary == fallback {
        return Err(EguiError::InvalidParam {
            what: "font families".to_owned(),
            reason: "primary and fallback must differ".to_owned(),
        });
    }
    font_insert(
        name,
        bytes,
        vec![
            InsertFontFamily {
                family: primary,
                priority: FontPriority::Highest,
            },
            InsertFontFamily {
                family: fallback,
                priority: FontPriority::Lowest,
            },
        ],
    )
}

/// Prepend the named font to both families of a definitions table.
///
/// Existing entries stay behind it as fallbacks (deduplicated, so a repeat
/// install never stacks the same name twice). Pure: the caller decides
/// whether the result reaches the `Context` via `set_fonts`.
pub fn with_bundled_font(
    mut definitions: egui::FontDefinitions,
    name: &str,
    bytes: &'static [u8],
) -> Result<egui::FontDefinitions, EguiError> {
    let name = checked_name(name)?;
    let bytes = checked_bytes(bytes)?;
    definitions.font_data.insert(
        name.clone(),
        std::sync::Arc::new(egui::FontData::from_static(bytes)),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        let chain = definitions.families.entry(family).or_default();
        chain.retain(|existing| existing != &name);
        chain.insert(0, name.clone());
    }
    Ok(definitions)
}

/// Queue the named font as the primary for both families on a live context.
///
/// The install takes effect at the start of the next pass (or immediately
/// when no text has been laid out yet this pass). Validation failures
/// refuse before anything is queued.
pub fn install_bundled_font(
    ctx: &egui::Context,
    name: &str,
    bytes: &'static [u8],
) -> Result<(), EguiError> {
    let insert = bundled_font_insert(name, bytes)?;
    ctx.add_font(insert);
    Ok(())
}

#[cfg(test)]
#[path = "../tests/fonts.rs"]
mod tests;
