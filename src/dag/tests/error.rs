use super::*;

#[test]
fn registry_error_display() {
    let err = RegistryError::DuplicateTarget {
        name: "build".into(),
    };
    assert!(format!("{err}").contains("build"));
    assert!(format!("{err}").contains("already exists"));
}

#[test]
fn target_not_found_display() {
    let err = RegistryError::TargetNotFound("missing".into());
    assert!(format!("{err}").contains("missing"));
    assert!(format!("{err}").contains("not found"));
}

#[test]
fn invalid_capability_display() {
    let err = RegistryError::InvalidCapability("bogus:cap".into());
    assert!(format!("{err}").contains("bogus:cap"));
    assert!(format!("{err}").contains("invalid capability"));
}
