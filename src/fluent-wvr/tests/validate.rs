use fluent_wvr::validate::{check_for_execute, Validate};
use fluent_wvr::{WorkError, WorkOutput};

#[test]
fn stub_validates() {
    struct Good;
    struct Bad;
    impl Validate for Good {
        type Err = String;
        fn validate(&self) -> Result<(), String> {
            Ok(())
        }
    }
    impl Validate for Bad {
        type Err = String;
        fn validate(&self) -> Result<(), String> {
            Err("empty symbol".to_owned())
        }
    }
    assert!(Good.validate().is_ok());
    assert!(check_for_execute(&Good, "ctx").is_ok());
    let err = check_for_execute(&Bad, "ctx").expect_err("fails");
    assert_eq!(
        err,
        WorkError::execution_context("ctx", "empty symbol"),
        "context stored as data, not baked into message"
    );
    let out: WorkOutput = WorkOutput::ok("ok");
    assert!(out.success);
    assert_eq!(out.message, "ok");
}
