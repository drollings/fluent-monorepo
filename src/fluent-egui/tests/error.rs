use super::*;

#[test]
fn invalid_param_reports_what_and_reason() {
    let err = EguiError::InvalidParam { what: "height".to_owned(), reason: "must be positive".to_owned() };
    assert_eq!(err.to_string(), "invalid parameter for height: must be positive");
}

#[test]
fn dependency_reports_reason() {
    let err = EguiError::Dependency { reason: "ribbon needs the eit pair".to_owned() };
    assert_eq!(err.to_string(), "dependency refused: ribbon needs the eit pair");
}

#[test]
fn error_is_clone_and_comparable() {
    let err = EguiError::InvalidParam { what: "spacing".to_owned(), reason: "must be finite".to_owned() };
    assert_eq!(err.clone(), err);
    assert_ne!(
        err,
        EguiError::Dependency { reason: "must be finite".to_owned() }
    );
}

#[test]
fn error_is_debug_renderable() {
    let err = EguiError::InvalidParam { what: "range".to_owned(), reason: "min must be below max".to_owned() };
    let rendered = format!("{err:?}");
    assert!(rendered.contains("InvalidParam"), "debug names the variant: {rendered}");
    assert!(rendered.contains("range"), "debug carries the field: {rendered}");
}
