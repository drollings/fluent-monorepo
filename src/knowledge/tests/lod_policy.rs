use super::*;
use crate::lod::Lod;

#[test]
fn lod_spec_full_band_and_midpoint() {
    let band = LodSpec::full();
    assert_eq!((band.min, band.max), (Lod::LOD1, Lod::LOD5));
    assert_eq!(band.midpoint(), Lod::LOD3);
}

#[test]
fn lod_spec_interpolate_bounds() {
    let band = LodSpec::full();
    // Single node → finest; most relevant of many → finest; least → coarsest.
    assert_eq!(band.interpolate(0, 1), Lod::LOD1);
    assert_eq!(band.interpolate(0, 5), Lod::LOD1);
    assert_eq!(band.interpolate(4, 5), Lod::LOD5);
    // Monotonic non-decreasing with rank.
    let mut prev = Lod::LOD1;
    for r in 0..9 {
        let lod = band.interpolate(r, 9);
        assert!(lod >= prev, "rank {r} regressed");
        assert!(lod >= band.min && lod <= band.max);
        prev = lod;
    }
}

#[test]
fn prompt_budget_intermediate_saturates() {
    let b = PromptBudget {
        max_chars: 100,
        reserve_head_tail: 30,
    };
    assert_eq!(b.intermediate_budget(20), 70, "bounded by reserve");
    assert_eq!(b.intermediate_budget(50), 50, "bounded by actual");
    assert_eq!(b.intermediate_budget(200), 0, "saturates, never wraps");
    assert_eq!(PromptBudget::from_tokens(100, 4).max_chars, 400);
    assert_eq!(PromptBudget::from_tokens_default(8192).max_chars, 32768);
}

#[test]
fn worker_context_system_prompt() {
    let w = WorkerContext::new("code reviewer", "Be terse.");
    assert_eq!(w.system_prompt(), "You are a code reviewer.\n\nBe terse.");
    assert_eq!(WorkerContext::default().system_prompt(), "");
    assert_eq!(
        WorkerContext::new("", "Only this.").system_prompt(),
        "Only this."
    );
}

#[test]
fn parse_tiers_happy_and_fail_open() {
    let (s, d) = parse_tiers("SUMMARY: hi\nDESCRIPTION: yo");
    assert_eq!(s.as_deref(), Some("hi"));
    assert_eq!(d.as_deref(), Some("yo"));
    let (s, d) = parse_tiers("plain text no delimiters");
    assert_eq!(s.as_deref(), Some("plain text no delimiters"));
    assert_eq!(d, None);
    let (s, d) = parse_tiers("");
    assert_eq!(s.as_deref(), Some(""));
    assert_eq!(d, None);
    let (s, _) = parse_tiers("SUMMARY: first\nSUMMARY: second");
    assert_eq!(s.as_deref(), Some("second"));
}
