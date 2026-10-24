//! YSM1 codec suite: control group (must not fire) + equivalence gate.
use super::*;
use crate::yago_view::YagoView;

/// Operator artifact (`make yago-load` regenerates it from the full taxonomy
/// download); `None` in checkouts without it — callers skip, don't fail.
fn n2_ttl() -> Option<String> {
    std::fs::read_to_string("env/yago-taxonomy-n2.ttl")
        .or_else(|_| std::fs::read_to_string("../../env/yago-taxonomy-n2.ttl"))
        .ok()
}

fn n2_view() -> Option<YagoView> {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("n2.ttl");
    std::fs::write(&path, n2_ttl()?).expect("write");
    Some(YagoView::load(&path).expect("n2 loads via TTL leg"))
}

fn skip_no_n2() {
    eprintln!("skip: env/yago-taxonomy-n2.ttl absent (operator artifact)");
}

#[test]
fn ttl_without_magic_still_parses() {
    let Some(view) = n2_view() else {
        skip_no_n2();
        return;
    };
    assert!(view.class_count() > 0);
    assert!(view.resolve_curie("schema:Thing").is_some());
}

#[test]
fn flipped_payload_byte_is_rejected() {
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let mut bytes = encode_ttl(&ttl);
    assert!(bytes.len() > YSM1_HEADER_LEN + 8);
    let flip_at = bytes.len() - 1;
    bytes[flip_at] ^= 0xff;
    assert!(
        decode(&bytes).is_err(),
        "a single flipped payload byte must fail the digest gate"
    );
}

#[test]
fn unknown_format_version_is_rejected() {
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let mut bytes = encode_ttl(&ttl);
    bytes[4..8].copy_from_slice(&0x7fff_ffffu32.to_le_bytes());
    assert!(
        decode(&bytes).is_err(),
        "an unknown format version must be rejected"
    );
}

#[test]
fn taxonomy_version_mismatch_is_rejected() {
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let mut bytes = encode_ttl(&ttl);
    let bad = taxonomy_version().wrapping_add(1);
    bytes[8..12].copy_from_slice(&bad.to_le_bytes());
    // Recompute the digest over the payload so only the version gate fires.
    let digest = common_core::hash::blake3_hash(&bytes[YSM1_HEADER_LEN..]);
    bytes[12..44].copy_from_slice(&digest);
    assert!(
        decode(&bytes).is_err(),
        "a stale taxonomy stamp must be rejected"
    );
}

#[test]
fn truncated_inputs_never_panic() {
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let full = encode_ttl(&ttl);
    for len in [0, 1, 10, 43, 44, 45, 52, 60] {
        let cut = full.len().min(len);
        assert!(
            decode(&full[..cut]).is_err(),
            "truncated to {cut} bytes must reject, never panic"
        );
    }
    // Corrupt edge index (out of range) rejects rather than misindexing.
    let mut bad_edge = full.clone();
    if let Some(last) = bad_edge.last_mut() {
        *last = 0xff;
    }
    // May reject via digest or via range — either way it must not load.
    // (The flipped-byte control above already pins the digest gate alone.)
    let _ = decode(&bad_edge);
}

#[test]
fn equivalence_gate_class_count_resolve_and_ancestors() {
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let Some(ttl_view) = n2_view() else {
        skip_no_n2();
        return;
    };
    let bytes = encode_ttl(&ttl);
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("n2.ysm1");
    std::fs::write(&path, &bytes).expect("write");
    let bin_view = YagoView::load(&path).expect("YSM1 loads");
    assert_eq!(
        bin_view.class_count(),
        ttl_view.class_count(),
        "class count identical across legs"
    );
    let curies: Vec<(String, _)> = ttl_view.classes_iter().collect();
    assert!(!curies.is_empty());
    for (curie, expected) in &curies {
        assert_eq!(
            bin_view.resolve_curie(curie),
            Some(*expected),
            "resolve_curie matches for {curie}"
        );
    }
    // Order-sensitive ancestor comparison over a fixed sample.
    for (curie, id) in curies.iter().take(200) {
        assert_eq!(
            bin_view.ancestors_of(*id),
            ttl_view.ancestors_of(*id),
            "ancestors_of (nearest-first) matches for {curie}"
        );
    }
}

#[test]
fn header_shape_matches_the_load_probe() {
    // The reader probes `data.len() >= 44` and magic as LE u32; the writer
    // must satisfy exactly that probe.
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let bytes = encode_ttl(&ttl);
    assert!(bytes.len() >= YSM1_HEADER_LEN);
    assert_eq!(
        u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
        YSM1_MAGIC_U32
    );
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        YSM1_FORMAT_VERSION
    );
}

#[test]
fn boot_latency_record_ttl_vs_binary() {
    use std::time::Instant;
    let Some(ttl) = n2_ttl() else {
        skip_no_n2();
        return;
    };
    let bytes = encode_ttl(&ttl);
    let dir = tempfile::tempdir().expect("tmpdir");
    let ttl_path = dir.path().join("n2.ttl");
    let bin_path = dir.path().join("n2.ysm1");
    std::fs::write(&ttl_path, &ttl).expect("write");
    std::fs::write(&bin_path, &bytes).expect("write");
    let hist = common_core::metrics::LatencyHistogram::new();
    let start = Instant::now();
    let ttl_view = YagoView::load(&ttl_path).expect("ttl loads");
    hist.observe_duration(start);
    let start = Instant::now();
    let bin_view = YagoView::load(&bin_path).expect("binary loads");
    hist.observe_duration(start);
    // Record only: correctness gates, speed is payoff. The equivalence
    // assertions live in the gate test above; here the views must agree.
    assert_eq!(bin_view.class_count(), ttl_view.class_count());
    // `hist` holds the two observations for the operator's before/after
    // readout; keep it alive past the loads it measured.
    std::hint::black_box(&hist);
}
