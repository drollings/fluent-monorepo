//! Closed-vocabulary label enums: one table generating the enum plus its
//! string round-trip (`as_str` / `Display` / `FromStr`).
//!
//! Several crates name a fixed set of strings both ways (spacy-rs UPOS /
//! IOB / NER / dep labels; provenance tiers; namespaces). Hand-writing the
//! `Display` and `FromStr` matches separately per enum duplicates the table
//! and lets the two directions drift; this macro keeps one
//! `Variant = discriminant => "text"` table as the single source of truth
//! for the enum definition and both directions.
//!
//! The macro is dependency-free: error type and constructor come from the
//! caller, `Display`/`FromStr` resolve through absolute `::std` paths, and
//! only the table lives here. Container/variant attributes (`repr`, serde,
//! docs) pass through untouched, so wire formats never change.

/// Declare a closed string vocabulary.
///
/// ```ignore
/// fluent_types::label_enum! {
///     /// Doc comment on the enum.
///     #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
///     #[repr(u8)]
///     pub enum Level {
///         /// Doc comment on a variant.
///         Low = 0 => "low",
///         High = 1 => "high",
///     }
///     err_ty MyError,
///     err_ctor MyError::UnknownLevel,
///     normalize lowercase,
/// }
/// ```
///
/// This generates the enum plus `as_str()`, `Display` (via `as_str`), and
/// `FromStr`. `normalize` selects the `FromStr` input folding and must match
/// the type's documented contract exactly:
/// - `lowercase` — fold with `to_ascii_lowercase` before matching; the
///   unknown-label error carries the *folded* text.
/// - `uppercase` — fold with `to_ascii_uppercase`; the error carries the
///   folded text.
/// - `exact` — match the input verbatim (no folding); the error carries the
///   raw input.
///
/// `err_ctor` is a `path` to a tuple-variant constructor taking the owned
/// offending text (`String`).
#[macro_export]
macro_rules! label_enum {
    (
        $(#[$emeta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$vmeta:meta])*
                $variant:ident = $disc:expr => $text:literal
            ),* $(,)?
        }
        err_ty $err_ty:ty,
        err_ctor $err_ctor:path,
        normalize $norm:ident $(,)?
    ) => {
        $(#[$emeta])*
        $vis enum $name {
            $(
                $(#[$vmeta])*
                $variant = $disc,
            )*
        }

        impl $name {
            /// The canonical wire text of this label (the single source the
            /// `Display` impl and hand-written key helpers delegate to).
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $text,)*
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $err_ty;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $crate::label_enum!(@parse $norm, $name, $err_ctor, s, $($variant => $text,)*)
            }
        }
    };
    (@parse lowercase, $name:ident, $err_ctor:path, $s:ident, $($variant:ident => $text:literal,)*) => {{
        let folded = $s.to_ascii_lowercase();
        match folded.as_str() {
            $($text => Ok($name::$variant),)*
            other => Err($err_ctor(other.to_string())),
        }
    }};
    (@parse uppercase, $name:ident, $err_ctor:path, $s:ident, $($variant:ident => $text:literal,)*) => {{
        let folded = $s.to_ascii_uppercase();
        match folded.as_str() {
            $($text => Ok($name::$variant),)*
            other => Err($err_ctor(other.to_string())),
        }
    }};
    (@parse exact, $name:ident, $err_ctor:path, $s:ident, $($variant:ident => $text:literal,)*) => {
        match $s {
            $($text => Ok($name::$variant),)*
            other => Err($err_ctor(other.to_string())),
        }
    };
}

#[cfg(test)]
#[path = "../tests/label_enum.rs"]
mod tests;
