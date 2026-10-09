use super::*;
use crate::money::{Money, Qty};

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

#[test]
fn money_and_quantity_parse_errors_name_their_type() {
    let err = "not-money".parse::<Money>().expect_err("bad money");
    assert!(
        matches!(err, TypesError::InvalidMoney(_)),
        "money failures must not hide behind InvalidBar, got: {err:?}"
    );
    assert!(err.to_string().contains("not-money"));
    let err = "not-a-count".parse::<Qty>().expect_err("bad quantity");
    assert!(
        matches!(err, TypesError::InvalidQuantity(_)),
        "quantity failures must not hide behind InvalidBar, got: {err:?}"
    );
    assert!("12.5".parse::<Money>().is_ok());
    assert!("3".parse::<Qty>().is_ok());
}
