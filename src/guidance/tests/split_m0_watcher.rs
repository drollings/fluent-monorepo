//! Watcher schedule identity: inline math equals shared math.

use common_core::retry::capped_exp_backoff_ms;

#[test]
fn watcher_delay_identity() {
    // Current inline shape: 100 * 2^min(consecutive-1, 8), capped at 5000.
    for consecutive in 1..=12u32 {
        let inline = 100u64
            .saturating_mul(2u64.pow(consecutive.saturating_sub(1).min(8)))
            .min(5_000);
        let shared = capped_exp_backoff_ms(100, consecutive - 1, 8).min(5_000);
        assert_eq!(inline, shared, "consecutive={consecutive}");
    }
}
