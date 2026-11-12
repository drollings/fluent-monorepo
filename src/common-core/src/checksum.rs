//! Zero-domain checksum primitives (no domain words: node/session/target/...).
//!
//! Home for pure digit/checksum validators shared across crates. Callers that
//! need a confidence gate on a checksum match (e.g. a credit-card regex firing
//! only when the digits validate) compose this; the regex/policy stays local.

/// Luhn checksum validation over the ASCII digits in `input`.
///
/// Non-digit characters (spaces, dashes) are ignored; the digit run must be
/// 13–19 digits long (the PAN range) and the checksum must hold.
///
/// # Examples
///
/// ```
/// use common_core::checksum::luhn_valid;
///
/// assert!(luhn_valid("4111111111111111"));
/// assert!(luhn_valid("4111-1111-1111-1111"));
/// assert!(!luhn_valid("1234567812345678"));
/// assert!(!luhn_valid(""));
/// ```
#[must_use]
/// Zero-domain checksum primitives (no domain words: node/session/target/...).
/// 
/// Home for pure digit/checksum validators shared across crates. Callers that
/// need a confidence gate on a checksum match (e.g. a credit-card regex firing
/// only when the digits validate) compose this; the regex/policy stays local.
/// Luhn checksum validation over the ASCII digits in `input`.
/// 
/// Non-digit characters (spaces, dashes) are ignored; the digit run must be
/// 13–19 digits long (the PAN range) and the checksum must hold.
/// 
/// # Examples
/// 
/// ```
/// use common_core::checksum::luhn_valid;
/// 
/// assert!(luhn_valid("4111111111111111"));
/// assert!(luhn_valid("4111-1111-1111-1111"));
/// assert!(!luhn_valid("1234567812345678"));
/// assert!(!luhn_valid(""));
/// ```
pub fn luhn_valid(input: &str) -> bool {
    let digits: Vec<u32> = input
        .chars()
        .filter(char::is_ascii_digit)
        .filter_map(|c| c.to_digit(10))
        .collect();
    if digits.len() < 13 || digits.len() > 19 {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let n = d * 2;
                if n > 9 {
                    n - 9
                } else {
                    n
                }
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}