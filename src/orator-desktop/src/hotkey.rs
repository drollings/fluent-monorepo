//! Hotkey chord codec: parse, validate, pretty-print, reserved check.
//!
//! One codec shared by configuration, the portal registration path, and the
//! settings surface, so a chord means the same string everywhere.

use std::collections::BTreeSet;

/// Modifier keys in canonical order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Modifier {
    /// Control.
    Ctrl,
    /// Alt/Option.
    Alt,
    /// Super/Windows/Command.
    Super,
    /// Shift.
    Shift,
}

/// A parsed hotkey chord: modifiers plus one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    /// Active modifiers, canonicalized.
    pub modifiers: BTreeSet<Modifier>,
    /// The key (`a`–`z`, `0`–`9`, `F1`–`F12`, …), lowercase.
    pub key: String,
}

/// Parse `Ctrl-Alt-d`-style chords. Modifiers in any order, case
/// insensitive, `-` or `+` separators. Returns `None` on empty keys,
/// lone modifiers, or unknown modifier names.
#[must_use]
pub fn parse_chord(text: &str) -> Option<Chord> {
    let mut modifiers = BTreeSet::new();
    let mut key: Option<String> = None;
    for part in text.split(['-', '+']) {
        let part = part.trim().to_lowercase();
        if part.is_empty() {
            return None;
        }
        match part.as_str() {
            "ctrl" | "control" => {
                modifiers.insert(Modifier::Ctrl);
            }
            "alt" | "option" => {
                modifiers.insert(Modifier::Alt);
            }
            "super" | "win" | "cmd" | "command" | "meta" => {
                modifiers.insert(Modifier::Super);
            }
            "shift" => {
                modifiers.insert(Modifier::Shift);
            }
            other => {
                if key.is_some() {
                    return None;
                }
                key = Some(other.to_string());
            }
        }
    }
    key.filter(|key| !is_modifier_word(key))
        .map(|key| Chord { modifiers, key })
}

/// Whether `word` names a modifier rather than a key.
fn is_modifier_word(word: &str) -> bool {
    matches!(
        word,
        "ctrl" | "control" | "alt" | "option" | "super" | "win" | "cmd" | "command" | "meta"
            | "shift" | "fn"
    )
}

impl Chord {
    /// Canonical rendering: modifiers in fixed order, then the key.
    #[must_use]
    pub fn pretty(&self) -> String {
        let mut parts = Vec::new();
        for modifier in [
            Modifier::Ctrl,
            Modifier::Alt,
            Modifier::Super,
            Modifier::Shift,
        ] {
            if self.modifiers.contains(&modifier) {
                parts.push(match modifier {
                    Modifier::Ctrl => "Ctrl",
                    Modifier::Alt => "Alt",
                    Modifier::Super => "Super",
                    Modifier::Shift => "Shift",
                });
            }
        }
        let mut rendered = parts.join("+");
        if !rendered.is_empty() {
            rendered.push('+');
        }
        rendered.push_str(&self.key);
        rendered
    }

    /// Whether the chord collides with reserved system shortcuts and must
    /// be rejected at registration time. Bare Escape / single keys with
    /// no modifier belong to the app; system chords never bind.
    #[must_use]
    pub fn is_reserved(&self) -> bool {
        if self.modifiers.is_empty() {
            return true;
        }
        let has = |modifier: Modifier| self.modifiers.contains(&modifier);
        let key = self.key.as_str();
        // Clipboard and kill chords.
        if has(Modifier::Ctrl) && matches!(key, "c" | "v" | "x" | "z" | "a") {
            return true;
        }
        // Window and session chords.
        if has(Modifier::Alt) && key == "f4" {
            return true;
        }
        if has(Modifier::Super) && matches!(key, "l" | "d" | "t" | "w" | "q") {
            return true;
        }
        // Terminal and interrupt chords.
        if has(Modifier::Ctrl) && has(Modifier::Shift) && matches!(key, "c" | "v") {
            return true;
        }
        if has(Modifier::Ctrl) && key == "alt" {
            return true;
        }
        false
    }
}

/// Compositor custom-shortcut fallback: when the portal GlobalShortcuts
/// interface is absent (the reliable COSMIC path), the user binds these
/// commands to keys directly. Always available, never a handshake.
#[must_use]
pub fn fallback_bindings() -> Vec<(&'static str, &'static str)> {
    vec![
        ("orator ptt start", "begin push-to-talk"),
        ("orator ptt stop", "end push-to-talk"),
        ("orator toggle", "toggle dictation"),
    ]
}

#[cfg(test)]
#[path = "../tests/hotkey.rs"]
mod tests;
