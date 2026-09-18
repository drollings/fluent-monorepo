use common_core::checksum::luhn_valid;

// Characterization of the shared primitive (mirrors
// src/router/tests/filters_luhn.rs vectors + the M1.1 edge cases).

#[test]
fn valid_vectors() {
    assert!(luhn_valid("4111111111111111")); // visa
    assert!(luhn_valid("5500000000000004")); // mastercard
    assert!(luhn_valid("340000000000009")); // amex (15 digits)
    assert!(luhn_valid("0000000000000000")); // all zeros
}

#[test]
fn invalid_checksum() {
    assert!(!luhn_valid("1234567812345678"));
    assert!(!luhn_valid("4111111111111112"));
}

#[test]
fn separators_ignored() {
    assert!(luhn_valid("4111-1111-1111-1111"));
    assert!(luhn_valid("4111 1111 1111 1111"));
}

#[test]
fn length_bounds() {
    assert!(!luhn_valid(""));
    assert!(!luhn_valid("123"));
    assert!(!luhn_valid("411111111111")); // 12 digits
    assert!(luhn_valid("4222222222222")); // 13 digits (valid Luhn)
    assert!(!luhn_valid("41111111111111111111")); // 20 digits
    assert!(!luhn_valid("411111111111111111111")); // 21 digits
}

#[test]
fn non_digits() {
    assert!(!luhn_valid("abcd"));
    assert!(!luhn_valid("---- ---- ---- ----"));
    // Letters are ignored, leaving too few digits.
    assert!(!luhn_valid("4111a1111b1111c11"));
}
