use super::*;

#[test]
fn unknown_timeframe_display() {
    let err = TypesError::UnknownTimeframe("W1".to_owned());
    assert!(err.to_string().contains("W1"));
}

#[test]
fn io_converts_from_std_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
    let err = TypesError::from(io_err);
    assert!(matches!(err, TypesError::Io(_)));
}
