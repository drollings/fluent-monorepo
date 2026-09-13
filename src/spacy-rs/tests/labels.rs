use super::*;

#[test]
fn upos_display_and_fromstr_roundtrip() {
    for tag in Upos::UPOS {
        let text = tag.to_string();
        let back: Upos = text.parse().expect("parse");
        assert_eq!(back, *tag, "roundtrip {text}");
    }
}

#[test]
fn upos_fromstr_is_case_insensitive() {
    assert_eq!("NOUN".parse::<Upos>().unwrap(), Upos::Noun);
    assert_eq!("Noun".parse::<Upos>().unwrap(), Upos::Noun);
    assert_eq!("noun".parse::<Upos>().unwrap(), Upos::Noun);
}

#[test]
fn upos_unknown_rejected() {
    assert!(matches!(
        "not-a-tag".parse::<Upos>(),
        Err(SpacyError::UnknownPos(_))
    ));
}

#[test]
fn upos_no_tag_renders_empty() {
    assert_eq!(Upos::NoTag.to_string(), "");
    assert_eq!("".parse::<Upos>().unwrap(), Upos::NoTag);
}

#[test]
fn upos_ids_match_spacy_symbols() {
    assert_eq!(Upos::NoTag.id(), 0);
    assert_eq!(Upos::Adj.id(), 84);
    assert_eq!(Upos::Noun.id(), 92);
    assert_eq!(Upos::X.id(), 101);
    assert_eq!(Upos::Space.id(), 103);
}

#[test]
fn ent_iob_display_and_parse() {
    assert_eq!(EntIoB::Missing.to_string(), "");
    assert_eq!(EntIoB::Inside.to_string(), "I");
    assert_eq!(EntIoB::Outside.to_string(), "O");
    assert_eq!(EntIoB::Begin.to_string(), "B");
    assert_eq!("B".parse::<EntIoB>().unwrap(), EntIoB::Begin);
    assert!(matches!(
        "L".parse::<EntIoB>(),
        Err(SpacyError::InvalidEntIobText(_))
    ));
    assert!(EntIoB::from_id(3).is_ok());
    assert!(matches!(
        EntIoB::from_id(4),
        Err(SpacyError::InvalidEntIob(4))
    ));
}

#[test]
fn ner_type_roundtrip() {
    assert_eq!("PERSON".parse::<NerType>().unwrap(), NerType::Person);
    assert_eq!("person".parse::<NerType>().unwrap(), NerType::Person);
    assert_eq!(NerType::Person.to_string(), "PERSON");
    assert_eq!(NerType::Person.id(), 380);
    assert_eq!(NerType::Cardinal.id(), 397);
}

#[test]
fn dep_rel_reference_set_roundtrips() {
    // The walkthrough's required reference labels must exist verbatim.
    for label in [
        "nsubj", "aux", "root", "prep", "pcomp", "compound", "dobj", "quantmod", "pobj",
    ] {
        let rel: DepRel = label.parse().expect("reference label");
        assert_eq!(rel.to_string(), label);
    }
    assert_eq!("nsubj".parse::<DepRel>().unwrap(), DepRel::Nsubj);
    assert_eq!(DepRel::Nsubj.id(), 429);
    assert_eq!(DepRel::Root.id(), 449);
}

#[test]
fn dep_label_set_accepts_ud_and_reference() {
    let set = DepLabelSet::ud_default();
    assert!(set.contains("nsubj"));
    assert!(set.contains("compound"));
    assert!(set.contains("case"));
    assert!(!set.contains("bogus_relation"));
    // Membership is case-insensitive.
    assert!(set.contains("ROOT"));
    assert!(set.contains("NSUBJ"));
}

#[test]
fn dep_label_set_serde_roundtrip() {
    let set = DepLabelSet::ud_default();
    let json = serde_json::to_string(&set).expect("serialize");
    let back: DepLabelSet = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(back.to_sorted_vec(), set.to_sorted_vec());
    assert!(back.contains("compound"));
}

#[test]
fn dep_label_set_fromstr_display_roundtrip() {
    let set: DepLabelSet = "nsubj,root,compound".parse().expect("parse");
    assert_eq!(set.to_sorted_vec(), vec!["compound", "nsubj", "root"]);
    assert_eq!(set.to_string(), "compound,nsubj,root");
    let again: DepLabelSet = set.to_string().parse().expect("reparse");
    assert_eq!(again.to_sorted_vec(), set.to_sorted_vec());
    assert!("nsubj,bogus".parse::<DepLabelSet>().is_err());
}

// ── M4.1 characterization: case/normalization edges the macro must preserve ──

/// `EntIoB::from_str` is an EXACT match (no case folding, unlike Upos and
/// NerType): lowercase singletons are rejected, not reinterpreted.
#[test]
fn ent_iob_exact_match_rejects_lowercase() {
    for good in ["", "I", "O", "B"] {
        assert!(good.parse::<EntIoB>().is_ok(), "{good:?} must parse");
    }
    for bad in ["i", "o", "b", "b_"] {
        assert!(
            matches!(
                bad.parse::<EntIoB>(),
                Err(SpacyError::InvalidEntIobText(_))
            ),
            "{bad:?} must be rejected"
        );
    }
    assert_eq!("".parse::<EntIoB>().unwrap(), EntIoB::Missing);
    assert_eq!("I".parse::<EntIoB>().unwrap(), EntIoB::Inside);
    assert_eq!("O".parse::<EntIoB>().unwrap(), EntIoB::Outside);
}

/// Every NerType variant round-trips through its UPPERCASE text; unknown
/// text is rejected (never defaulted).
#[test]
fn ner_type_full_roundtrip_and_unknown() {
    for t in [
        NerType::Person,
        NerType::Norp,
        NerType::Facility,
        NerType::Org,
        NerType::Gpe,
        NerType::Loc,
        NerType::Product,
        NerType::Event,
        NerType::WorkOfArt,
        NerType::Language,
        NerType::Law,
        NerType::Date,
        NerType::Time,
        NerType::Percent,
        NerType::Money,
        NerType::Quantity,
        NerType::Ordinal,
        NerType::Cardinal,
    ] {
        let text = t.to_string();
        assert_eq!(text, text.to_ascii_uppercase(), "NerType renders UPPER");
        assert_eq!(text.parse::<NerType>().unwrap(), t, "roundtrip {text}");
        assert_eq!(
            text.to_ascii_lowercase().parse::<NerType>().unwrap(),
            t,
            "lowercase input folds up"
        );
    }
    assert!(matches!(
        "BOGUS".parse::<NerType>(),
        Err(SpacyError::UnknownNerType(_))
    ));
}

/// DepRel folds ASCII case like Upos; unknown labels are rejected.
#[test]
fn dep_rel_case_insensitive_and_unknown() {
    assert_eq!("NSUBJ".parse::<DepRel>().unwrap(), DepRel::Nsubj);
    assert_eq!("Nsubj".parse::<DepRel>().unwrap(), DepRel::Nsubj);
    assert_eq!("ROOT".parse::<DepRel>().unwrap(), DepRel::Root);
    assert!(matches!(
        "bogus_relation".parse::<DepRel>(),
        Err(SpacyError::UnknownDepLabel(_))
    ));
}

/// The Upos unknown-label error carries the NORMALIZED (lowercased) text,
/// not the raw input — the macro must reproduce this payload exactly.
#[test]
fn upos_error_carries_normalized_text() {
    assert!(matches!(
        "NOPE".parse::<Upos>(),
        Err(SpacyError::UnknownPos(payload)) if payload == "nope"
    ));
}

/// An empty/blank label list parses to the empty set (empties are filtered,
/// never rejected).
#[test]
fn dep_label_set_empty_string_parses_empty() {
    let set: DepLabelSet = "".parse().expect("empty parses");
    assert!(set.is_empty());
    assert_eq!(set.to_string(), "");
    let set: DepLabelSet = "  , ,".parse().expect("blank parses");
    assert!(set.is_empty());
}
