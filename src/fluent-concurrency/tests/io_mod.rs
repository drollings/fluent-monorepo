use super::*;

#[test]
fn test_capability_error_missing() {
    let err = CapabilityError::Missing { name: "fs" };
    assert_eq!(err.to_string(), "missing capability: fs");
    let io_err: std::io::Error = err.into();
    assert_eq!(io_err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(io_err.to_string().contains("missing capability: fs"));
}

#[test]
fn test_capability_error_exhausted() {
    let err = CapabilityError::Exhausted {
        name: "db",
        detail: "pool empty".into(),
    };
    let s = err.to_string();
    assert!(s.contains("exhausted"));
    assert!(s.contains("db"));
    assert!(s.contains("pool empty"));
    let io_err: std::io::Error = err.into();
    assert_eq!(io_err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(io_err.to_string().contains("exhausted"));
}

#[test]
fn test_capability_error_traits() {
    let a = CapabilityError::Missing { name: "x" };
    let _ = format!("{a:?}");
    let b = a.clone();
    assert_eq!(a, b);
    let c = CapabilityError::Missing { name: "y" };
    assert_ne!(a, c);
}

#[test]
fn test_capability_error_into_concurrency_error() {
    let cap_err = CapabilityError::Missing { name: "net" };
    let io_err: std::io::Error = cap_err.into();
    let conc_err: common_core::error::IoError = io_err.into();
    assert_eq!(conc_err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(conc_err.to_string().contains("missing"));
}
