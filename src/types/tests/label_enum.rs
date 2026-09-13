//! Macro unit tests for `fluent_types::label_enum`, independent of any call
//! site: three sample enums (one per normalization mode) pin the generated
//! `as_str` / `Display` / `FromStr` semantics, error payloads, discriminant
//! values, attribute passthrough (repr + serde + docs), and serde
//! round-trips.

#[derive(Debug, PartialEq)]
enum SampleError {
    UnknownLevel(String),
    UnknownMark(String),
    UnknownKind(String),
}

crate::label_enum! {
    /// Traffic level vocabulary.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[repr(u8)]
    enum Level {
        /// No traffic observed.
        Low = 0 => "low",
        High = 9 => "high",
    }
    err_ty SampleError,
    err_ctor SampleError::UnknownLevel,
    normalize lowercase,
}

label_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
    #[repr(u16)]
    #[serde(rename_all = "UPPERCASE")]
    enum Mark {
        Alpha = 380 => "ALPHA",
        BetaGlyph = 381 => "BETA_GLYPH",
    }
    err_ty SampleError,
    err_ctor SampleError::UnknownMark,
    normalize uppercase,
}

label_enum! {
    /// Exact-match markers (no folding).
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    #[repr(u8)]
    enum Sigil {
        /// Missing annotation renders empty.
        Missing = 0 => "",
        Inside = 1 => "I",
    }
    err_ty SampleError,
    err_ctor SampleError::UnknownKind,
    normalize exact,
}

#[test]
fn lowercase_folds_and_reports_folded_text() {
    assert_eq!(Level::Low.as_str(), "low");
    assert_eq!(Level::High.to_string(), "high");
    assert_eq!("LOW".parse::<Level>(), Ok(Level::Low));
    assert_eq!("Low".parse::<Level>(), Ok(Level::Low));
    assert_eq!("high".parse::<Level>(), Ok(Level::High));
    // The error carries the FOLDED text, matching the hand-written convention.
    assert_eq!(
        "NOPE".parse::<Level>(),
        Err(SampleError::UnknownLevel("nope".to_string()))
    );
    // Discriminants survive the macro.
    assert_eq!(Level::Low as u8, 0);
    assert_eq!(Level::High as u8, 9);
}

#[test]
fn uppercase_folds_up_and_keeps_serde_shape() {
    assert_eq!(Mark::BetaGlyph.as_str(), "BETA_GLYPH");
    assert_eq!("beta_glyph".parse::<Mark>(), Ok(Mark::BetaGlyph));
    assert_eq!(
        "nope".parse::<Mark>(),
        Err(SampleError::UnknownMark("NOPE".to_string()))
    );
    assert_eq!(Mark::Alpha as u16, 380);
    // `rename_all = "UPPERCASE"` passes through verbatim: serde uppercases
    // the variant ident as-is (`BetaGlyph` → `BETAGLYPH`, no underscores —
    // which is exactly why real vocabularies pin explicit wire strings in
    // the table instead of relying on the rename rule).
    let json = serde_json::to_string(&Mark::BetaGlyph).expect("serialize");
    assert_eq!(json, "\"BETAGLYPH\"");
    let back: Mark = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back, Mark::BetaGlyph);
}

#[test]
fn exact_matches_verbatim_and_rejects_case_variants() {
    assert_eq!(Sigil::Missing.as_str(), "");
    assert_eq!("".parse::<Sigil>(), Ok(Sigil::Missing));
    assert_eq!("I".parse::<Sigil>(), Ok(Sigil::Inside));
    assert_eq!(
        "i".parse::<Sigil>(),
        Err(SampleError::UnknownKind("i".to_string()))
    );
    assert_eq!(Sigil::Inside as u8, 1);
}
