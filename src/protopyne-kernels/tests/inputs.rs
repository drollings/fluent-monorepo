use super::*;

fn length_def() -> InputDef {
    InputDef::Float {
        default: 14.0,
        min: 1.0,
        max: 100.0,
    }
}

#[test]
fn declare_exposes_default_and_accepts_in_bounds_set() {
    let mut inputs = InputSet::new();
    inputs.declare("length", length_def()).expect("valid def");
    assert_eq!(inputs.get_float("length"), Some(14.0));
    inputs
        .set("length", PineValue::Float(20.0))
        .expect("in bounds");
    assert_eq!(inputs.get_float("length"), Some(20.0));
}

#[test]
fn declare_rejects_bad_bounds_and_duplicates() {
    let mut inputs = InputSet::new();
    assert!(inputs
        .declare(
            "bad",
            InputDef::Float {
                default: 5.0,
                min: 10.0,
                max: 1.0,
            }
        )
        .is_err());
    assert!(inputs
        .declare(
            "outside",
            InputDef::Int {
                default: 500,
                min: 1,
                max: 100,
            }
        )
        .is_err());
    assert!(inputs
        .declare("nan", InputDef::Float {
            default: f64::NAN,
            min: 0.0,
            max: 1.0,
        })
        .is_err());
    inputs.declare("length", length_def()).expect("valid def");
    assert!(inputs.declare("length", length_def()).is_err());
}

#[test]
fn set_rejects_unknown_kind_mismatch_and_out_of_bounds() {
    let mut inputs = InputSet::new();
    inputs.declare("length", length_def()).expect("valid def");
    assert!(inputs.set("nope", PineValue::Float(1.0)).is_err());
    assert!(inputs.set("length", PineValue::Int(5)).is_err());
    assert!(inputs.set("length", PineValue::Float(101.0)).is_err());
    assert!(inputs.set("length", PineValue::Float(0.5)).is_err());
    assert_eq!(inputs.get_float("length"), Some(14.0));
}
