//! Doctor tests: the report never fails, only informs.

use crate::doctor::doctor_report;

#[test]
fn report_always_resolves() {
    let report = doctor_report();
    assert!(report.opus_ok, "linked opus must pass its self-test");
    assert!(
        report
            .models_dir
            .as_ref()
            .is_some_and(|dir| dir.ends_with("orator/models")),
        "managed models dir resolves under a home base"
    );
    if report.devices.is_empty() {
        assert!(
            !report.notes.is_empty(),
            "no devices must come with guidance"
        );
    }
}
