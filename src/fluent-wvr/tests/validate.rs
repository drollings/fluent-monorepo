use fluent_wvr::validate::{check_for_execute, valid_output, Validate};
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
        WorkError::Execution("ctx: empty symbol".to_owned()),
        "context prefix preserved"
    );
    let out: WorkOutput = valid_output("ok");
    assert!(out.success);
    assert_eq!(out.message, "ok");
}
