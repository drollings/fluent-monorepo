use super::*;

use skrifa::MetadataProvider as _;

const SANS: &[u8] = include_bytes!("../fonts/NotoSans-Regular.ttf");
const MONO: &[u8] = include_bytes!("../fonts/NotoSansMono-Regular.ttf");

#[test]
fn noto_names_are_distinct_and_usable() {
    assert!(!NOTO_SANS_NAME.trim().is_empty());
    assert!(!NOTO_SANS_MONO_NAME.trim().is_empty());
    assert_ne!(NOTO_SANS_NAME, NOTO_SANS_MONO_NAME);
}

#[test]
fn bundled_bytes_are_real_sfnt_files() {
    for bytes in [SANS, MONO] {
        assert!(bytes.len() > 100_000, "bundle ships a real font file");
        // sfnt version 1.0 (`0x00010000`): TrueType outlines.
        assert_eq!(&bytes[0..4], &[0x00, 0x01, 0x00, 0x00]);
    }
}

/// Does `bytes` map `glyph` to a real glyph (not `.notdef`)?
///
/// `egui :: has_glyph` cannot answer this: it false-negatives whenever the
/// glyph shares its face with the replacement character, which is exactly
/// the mono bundle's shape. The charmap is the honest predicate.
fn covers(bytes: &[u8], glyph: char) -> bool {
    let font = skrifa::FontRef::from_index(bytes, 0).expect("bundle parses");
    font.charmap()
        .map(glyph)
        .is_some_and(|id| id != skrifa::GlyphId::NOTDEF)
}

#[test]
fn bundle_covers_terminal_symbols() {
    // Glyphs the terminal paints every frame: accents, currency, the replay
    // `→`, month/replay `◀`/`▶`, the pulldown `▾`, Greek, and math. Noto
    // Sans itself lacks the arrows, shapes, chevron, and `√` — the mono
    // bundle's proportional-fallback leg covers them, so each glyph must
    // live in at least one of the two files.
    for glyph in [
        'é', '€', '£', '¥', '→', '◀', '▶', '▾', 'Δ', '√', '×', '−', '•', '…', '–',
    ] {
        assert!(
            covers(SANS, glyph) || covers(MONO, glyph),
            "{glyph:?} (U+{:04X}) is in neither bundle file",
            glyph as u32,
        );
    }
    // The split is pinned too, so a future file swap cannot silently move
    // coverage: arrows, shapes, and the chevron ride the mono file, prose
    // rides sans.
    for glyph in ['→', '◀', '▶', '▾', '√'] {
        assert!(covers(MONO, glyph), "mono backstop lacks {glyph:?}");
    }
    for glyph in ['é', '€', 'Δ'] {
        assert!(covers(SANS, glyph), "sans primary lacks {glyph:?}");
    }
}

#[test]
fn noto_sans_is_primary_after_install() {
    let ctx = egui::Context::default();
    install_noto_sans(&ctx, SANS, MONO).expect("bundle installs");
    // One pass applies the queued installs.
    let mut output = ctx.run_ui(egui::RawInput::default(), |_| {});
    output.textures_delta.clear();
    for (family, expected) in [
        (egui::FontFamily::Proportional, NOTO_SANS_NAME),
        (egui::FontFamily::Monospace, NOTO_SANS_MONO_NAME),
    ] {
        let chain = ctx.fonts(|fonts| {
            fonts
                .definitions()
                .families
                .get(&family)
                .cloned()
                .unwrap_or_default()
        });
        assert_eq!(
            chain.first().map(String::as_str),
            Some(expected),
            "bundle is the primary for {family:?}"
        );
        assert!(
            chain.len() > 1,
            "previous defaults stay behind as fallbacks for {family:?}"
        );
    }
    // The mono bundle's proportional-fallback leg: the install queues it
    // before the sans primary because `add_font` skips a name it already
    // holds, so the leg must ride the first insert to take effect at all.
    let chain = ctx.fonts(|fonts| {
        fonts
            .definitions()
            .families
            .get(&egui::FontFamily::Proportional)
            .cloned()
            .unwrap_or_default()
    });
    assert!(
        chain.contains(&NOTO_SANS_MONO_NAME.to_owned()),
        "mono backstops proportional text: {chain:?}"
    );
}

#[test]
fn empty_bundle_bytes_are_refused() {
    let ctx = egui::Context::default();
    assert!(install_noto_sans(&ctx, b"", MONO).is_err());
    assert!(install_noto_sans(&ctx, SANS, b"").is_err());
}
