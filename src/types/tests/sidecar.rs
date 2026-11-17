use super::SidecarHeader;
use std::collections::BTreeMap;

fn header() -> SidecarHeader {
    SidecarHeader {
        src: "a.rs".to_string(),
        body: "b".to_string(),
        src_hash: Some("s".to_string()),
        stuck: false,
        components: BTreeMap::from([("toolchain".to_string(), "t".to_string())]),
    }
}

#[test]
fn clean_receipts_keep_legacy_shape() {
    let line = header().render();
    assert!(!line.contains("stuck"), "{line}");
    let back = SidecarHeader::parse_line(&line).expect("parses");
    assert!(!back.stuck);
    assert_eq!(back.components.len(), 1);
}

#[test]
fn stuck_round_trips_and_reads_stale() {
    let mut stuck = header();
    stuck.stuck = true;
    let line = stuck.render();
    assert!(line.contains("\tstuck=1"), "{line}");
    let back = SidecarHeader::parse_line(&line).expect("parses");
    assert!(back.stuck);
    assert_eq!(back.src_hash.as_deref(), Some("s"));
}

#[test]
fn malformed_stuck_values_refuse_the_line() {
    for bad in [
        "GHDR1 src=a\tbody=b\tstuck=banana",
        "GHDR1 src=a\tbody=b\tstuck=0",
        "GHDR1 src=a\tbody=b\tstuck=",
    ] {
        assert!(
            SidecarHeader::parse_line(bad).is_none(),
            "stuck value must be exactly `1` or absent: {bad}"
        );
    }
    // Valid receipts still parse byte-identically.
    let clean = "GHDR1 src=a\tbody=b\tcomp=";
    let back = SidecarHeader::parse_line(clean).expect("parses");
    assert!(!back.stuck);
    let stuck = "GHDR1 src=a\tbody=b\tstuck=1\tcomp=";
    assert!(SidecarHeader::parse_line(stuck).expect("parses").stuck);
}
