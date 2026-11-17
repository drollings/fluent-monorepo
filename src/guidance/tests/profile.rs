use super::{count, report, Phase};

#[test]
fn phase_timer_reports_elapsed_without_panicking() {
    let phase = Phase::new("test.phase");
    std::thread::sleep(std::time::Duration::from_millis(1));
    assert!(phase.elapsed_ms() < 60_000);
    report("test.phase", 3);
    count("test.count", 7);
}
