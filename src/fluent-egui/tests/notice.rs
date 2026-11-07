use super::*;
use std::time::Duration;

fn notice(key: &str, text: &str, dwell_ms: u64) -> Notice {
    Notice::new()
        .text(text.to_owned())
        .dedupe_key(key.to_owned())
        .dwell(dwell_ms)
        .build()
}

#[test]
fn identical_keys_refresh_without_duplicating() {
    let center = NoticeCenter::new(4).expect("valid center");
    center.offer(notice("flap", "first", 1000)).expect("offers");
    center
        .offer(notice("flap", "second", 1000))
        .expect("offers");
    assert_eq!(center.len(), 1, "the flapping key coalesces");
    let live = center.notices();
    assert_eq!(live.len(), 1);
    assert_eq!(live[0].text, "second", "the latest burst wins");
}

#[test]
fn distinct_keys_never_merge() {
    let center = NoticeCenter::new(4).expect("valid center");
    for key in ["alpha", "beta", "gamma"] {
        center.offer(notice(key, key, 1000)).expect("offers");
    }
    assert_eq!(center.len(), 3, "distinct keys stay distinct");
    let mut keys: Vec<String> = center
        .notices()
        .iter()
        .map(|notice| notice.dedupe_key.clone())
        .collect();
    keys.sort();
    assert_eq!(keys, vec!["alpha", "beta", "gamma"]);
}

#[test]
fn blocked_intake_refuses_and_stays_bounded() {
    let center = NoticeCenter::new(2).expect("valid center");
    center.offer(notice("a", "a", 60_000)).expect("fits");
    center.offer(notice("b", "b", 60_000)).expect("fits");
    let mut refused = 0;
    for index in 0..10_000 {
        if center
            .offer(notice(&format!("burst-{index}"), "burst", 60_000))
            .is_err()
        {
            refused += 1;
        }
    }
    assert_eq!(refused, 10_000, "every over-limit offer refuses");
    assert!(
        center.len() <= 2,
        "ten thousand offers keep len <= 2: {}",
        center.len()
    );
}

#[test]
fn expiry_and_explicit_close_pop_exactly_one_entry() {
    let center = NoticeCenter::new(4).expect("valid center");
    center.offer(notice("short", "short", 50)).expect("offers");
    center
        .offer(notice("long", "long", 3_600_000))
        .expect("offers");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(center.expire_due(), 1, "expiry pops the due notice only");
    assert_eq!(center.len(), 1, "one entry left standing");
    assert!(center.close("long"), "explicit close pops it");
    assert!(center.is_empty(), "nothing left standing");
    assert!(!center.close("long"), "closing twice reports nothing left");
    assert!(!center.close("missing"), "unknown keys report nothing");
}

#[test]
fn offsets_stack_rows_by_stride() {
    assert_eq!(offsets(3, 24.0, 4.0), vec![0.0, 28.0, 56.0]);
    assert!(offsets(0, 24.0, 4.0).is_empty(), "no rows, no offsets");
    assert_eq!(
        offsets(2, -5.0, 4.0),
        vec![0.0, 4.0],
        "a degenerate height reads as zero, the gap still strides"
    );
}

#[test]
fn malformed_notices_refuse_before_intake() {
    let center = NoticeCenter::new(4).expect("valid center");
    let blank = Notice::new()
        .text("   ".to_owned())
        .dedupe_key("x".to_owned())
        .dwell(1000)
        .build();
    assert!(center.offer(blank).is_err(), "blank text refuses");
    let instant = Notice::new()
        .text("hi".to_owned())
        .dedupe_key("y".to_owned())
        .dwell(0)
        .build();
    assert!(center.offer(instant).is_err(), "zero dwell refuses");
    assert!(center.is_empty(), "refusals never reach the queue");
}
