//! Unit tests for [`crate::error`].

use crate::error::AudioError;

#[test]
fn errors_name_the_failure() {
    assert!(AudioError::NoDevice("x".to_string())
        .to_string()
        .contains("no audio device"));
    assert!(AudioError::Archive("full".to_string())
        .to_string()
        .contains("archive"));
}
