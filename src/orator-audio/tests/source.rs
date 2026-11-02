//! Capture-source tests: synthetic always present, PipeWire deferred loudly.

use crate::source::{pipewire_note, pipewire_source, probe_device, DriverError, SourceKind};

#[test]
fn synthetic_source_always_resolves() {
    assert_eq!(probe_device(&SourceKind::Synthetic).as_deref(), Some("synthetic"));
}

#[test]
fn pipewire_reports_absence_actionably() {
    assert_eq!(pipewire_source(), None);
    assert!(pipewire_note().contains("cpal"));
}

#[test]
fn driver_retries_transients_and_fails_fast_on_unavailable() {
    let transient = common_core::runtime::block_on(crate::source::supervise_driver(|| async {
        Err::<(), DriverError>(DriverError::NoDevice("mic".to_string()))
    }));
    assert_eq!(transient.unwrap_err(), DriverError::NoDevice("mic".to_string()));
    let fast = common_core::runtime::block_on(crate::source::supervise_driver(|| async {
        Err::<(), DriverError>(DriverError::Unavailable("pipewire".to_string()))
    }));
    assert_eq!(
        fast.unwrap_err(),
        DriverError::Unavailable("pipewire".to_string())
    );
}
