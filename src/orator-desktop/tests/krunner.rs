//! Runner tests: prefix match, offers, no ambient execution.

use crate::krunner::{parse_query, RunnerOffer};

#[test]
fn orator_prefix_parses_with_offers() {
    let query = parse_query("orator what time is it").expect("parses");
    assert_eq!(query.question, "what time is it");
    assert_eq!(
        query.offers[0],
        RunnerOffer::Ask("what time is it".to_string())
    );
    assert!(query.offers.contains(&RunnerOffer::StartMeeting));
    assert!(query.offers.contains(&RunnerOffer::Dictate));
}

#[test]
fn bare_prefix_offers_sessions_only() {
    let query = parse_query("orator").expect("parses");
    assert!(query.question.is_empty());
    assert!(!query.offers.iter().any(|offer| matches!(offer, RunnerOffer::Ask(_))));
}

#[test]
fn other_text_is_not_ours() {
    assert!(parse_query("firefox").is_none());
    assert!(parse_query("").is_none());
    assert!(parse_query("oratorian").is_none());
}
