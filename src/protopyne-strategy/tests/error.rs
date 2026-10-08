use super::*;

#[test]
fn messages_name_the_problem() {
    let t = StrategyError::InvalidTransition("x".to_owned());
    assert!(t.to_string().contains("x"));
    let g = StrategyError::InvalidGrant("y".to_owned());
    assert!(g.to_string().contains("y"));
    let e = StrategyError::InvalidElement("z".to_owned());
    assert!(e.to_string().contains("z"));
}
