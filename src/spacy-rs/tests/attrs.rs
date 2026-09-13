use super::*;

#[test]
fn ids_match_spacy_attrs() {
    assert_eq!(Attribute::IsAlpha.id(), 1);
    assert_eq!(Attribute::IsCurrency.id(), 18);
    assert_eq!(Attribute::Id.id(), 64);
    assert_eq!(Attribute::Orth.id(), 65);
    assert_eq!(Attribute::Lemma.id(), 73);
    assert_eq!(Attribute::Pos.id(), 74);
    assert_eq!(Attribute::Dep.id(), 76);
    assert_eq!(Attribute::EntIob.id(), 77);
    assert_eq!(Attribute::Head.id(), 79);
    assert_eq!(Attribute::SentStart.id(), 80);
    assert_eq!(Attribute::Idx.id(), 87);
}

#[test]
fn from_id_roundtrips() {
    for id in [0u16, 1, 18, 19, 63, 64, 73, 88, 100, 200] {
        let attr = Attribute::from_id(id);
        assert_eq!(attr.id(), id);
    }
}

#[test]
fn flag_boundary() {
    assert!(Attribute::IsAlpha.is_flag());
    assert!(Attribute::Other(19).is_flag());
    assert!(!Attribute::Other(64).is_flag());
    assert!(!Attribute::Orth.is_flag());
}

#[test]
fn from_name_case_insensitive() {
    assert_eq!(Attribute::from_name("orth").unwrap(), Attribute::Orth);
    assert_eq!(Attribute::from_name("ORTH").unwrap(), Attribute::Orth);
    assert_eq!(Attribute::from_name("ent_iob").unwrap(), Attribute::EntIob);
    assert!(matches!(
        Attribute::from_name("bogus"),
        Err(SpacyError::UnknownAttributeText(_))
    ));
}

/// M11.1: every `Attribute` variant resolves through `from_name` (exact
/// upper-case form) with the matching id — the agreement pin that makes a
/// variant-without-name-arm (the `IsGetWord` class of drift) fail CI. The
/// hardcoded length forces this list to grow with the enum.
#[test]
fn from_name_covers_every_variant() {
    const TABLE: &[(&str, Attribute, u16)] = &[
        ("IS_ALPHA", Attribute::IsAlpha, 1u16),
        ("IS_ASCII", Attribute::IsAscii, 2u16),
        ("IS_DIGIT", Attribute::IsDigit, 3u16),
        ("IS_LOWER", Attribute::IsLower, 4u16),
        ("IS_PUNCT", Attribute::IsPunct, 5u16),
        ("IS_SPACE", Attribute::IsSpace, 6u16),
        ("IS_TITLE", Attribute::IsTitle, 7u16),
        ("IS_UPPER", Attribute::IsUpper, 8u16),
        ("LIKE_URL", Attribute::LikeUrl, 9u16),
        ("LIKE_NUM", Attribute::LikeNum, 10u16),
        ("LIKE_EMAIL", Attribute::LikeEmail, 11u16),
        ("IS_STOP", Attribute::IsStop, 12u16),
        ("IS_OOV_DEPRECATED", Attribute::IsOovDeprecated, 13u16),
        ("IS_BRACKET", Attribute::IsBracket, 14u16),
        ("IS_QUOTE", Attribute::IsQuote, 15u16),
        ("IS_LEFT_PUNCT", Attribute::IsLeftPunct, 16u16),
        ("IS_RIGHT_PUNCT", Attribute::IsRightPunct, 17u16),
        ("IS_CURRENCY", Attribute::IsCurrency, 18u16),
        ("IS_DET_WORD", Attribute::IsDetWord, 19u16),
        ("IS_ADP_WORD", Attribute::IsAdpWord, 20u16),
        ("IS_AUX_WORD", Attribute::IsAuxWord, 21u16),
        ("IS_CCONJ_WORD", Attribute::IsCconjWord, 22u16),
        ("IS_SCONJ_WORD", Attribute::IsSconjWord, 23u16),
        ("IS_PRON_WORD", Attribute::IsPronWord, 24u16),
        ("IS_VERB_WORD", Attribute::IsVerbWord, 25u16),
        ("IS_BE_VERB", Attribute::IsBeVerb, 26u16),
        ("IS_BARE_INF_HOST", Attribute::IsBareInfHost, 27u16),
        ("IS_NEGATOR", Attribute::IsNegator, 28u16),
        ("IS_NOMINATIVE", Attribute::IsNominative, 29u16),
        ("IS_POSSESSIVE", Attribute::IsPossessive, 30u16),
        ("IS_RELATIVIZER", Attribute::IsRelativizer, 31u16),
        ("IS_SENSORY_VERB", Attribute::IsSensoryVerb, 32u16),
        ("IS_EPISTEMIC_VERB", Attribute::IsEpistemicVerb, 33u16),
        ("IS_DISCOURSE_MARKER", Attribute::IsDiscourseMarker, 34u16),
        ("IS_ADVERB_WORD", Attribute::IsAdverbWord, 35u16),
        ("IS_SUBORD_COMPLEMENT", Attribute::IsSubordComplement, 36u16),
        ("IS_SUBORD_ADVERBIAL", Attribute::IsSubordAdverbial, 37u16),
        ("IS_WHERE_WORD", Attribute::IsWhereWord, 38u16),
        ("IS_LOCATIVE", Attribute::IsLocative, 39u16),
        ("IS_DEMONSTRATIVE", Attribute::IsDemonstrative, 40u16),
        ("IS_TODAY_WORD", Attribute::IsTodayWord, 41u16),
        ("IS_AS_WORD", Attribute::IsAsWord, 42u16),
        ("IS_AFTER_WORD", Attribute::IsAfterWord, 43u16),
        ("IS_THAT_WORD", Attribute::IsThatWord, 44u16),
        ("IS_TWICE_WORD", Attribute::IsTwiceWord, 45u16),
        ("IS_YET_WORD", Attribute::IsYetWord, 46u16),
        ("IS_PLEASE_WORD", Attribute::IsPleaseWord, 47u16),
        ("IS_BE_CLITIC_S", Attribute::IsBeCliticS, 48u16),
        ("IS_BE_CLITIC", Attribute::IsBeClitic, 49u16),
        ("IS_THERE_WORD", Attribute::IsThereWord, 50u16),
        ("IS_WH_ADVERBIAL", Attribute::IsWhAdverbial, 51u16),
        ("IS_GET_WORD", Attribute::IsGetWord, 52u16),
        ("IS_HAVE_CLITIC", Attribute::IsHaveClitic, 53u16),
        ("IS_WILL_CLITIC", Attribute::IsWillClitic, 54u16),
        ("IS_INDEFINITE_PRONOUN", Attribute::IsIndefinitePronoun, 55u16),
        ("ID", Attribute::Id, 64u16),
        ("ORTH", Attribute::Orth, 65u16),
        ("LOWER", Attribute::Lower, 66u16),
        ("NORM", Attribute::Norm, 67u16),
        ("SHAPE", Attribute::Shape, 68u16),
        ("PREFIX", Attribute::Prefix, 69u16),
        ("SUFFIX", Attribute::Suffix, 70u16),
        ("LENGTH", Attribute::Length, 71u16),
        ("CLUSTER", Attribute::Cluster, 72u16),
        ("LEMMA", Attribute::Lemma, 73u16),
        ("POS", Attribute::Pos, 74u16),
        ("TAG", Attribute::Tag, 75u16),
        ("DEP", Attribute::Dep, 76u16),
        ("ENT_IOB", Attribute::EntIob, 77u16),
        ("ENT_TYPE", Attribute::EntType, 78u16),
        ("HEAD", Attribute::Head, 79u16),
        ("SENT_START", Attribute::SentStart, 80u16),
        ("SPACY", Attribute::Spacy, 81u16),
        ("PROB", Attribute::Prob, 82u16),
        ("LANG", Attribute::Lang, 83u16),
        ("ENT_KB_ID", Attribute::EntKbId, 84u16),
        ("MORPH", Attribute::Morph, 85u16),
        ("ENT_ID", Attribute::EntId, 86u16),
        ("IDX", Attribute::Idx, 87u16),
        ("SENT_END", Attribute::SentEnd, 88u16),
        ("INTERLINGUA_LEMMA_ID", Attribute::InterlinguaLemmaId, 89u16),
        ("INTERLINGUA_ENTITY_ID", Attribute::InterlinguaEntityId, 90u16),
        ("ANNOTATION_CONFIDENCE", Attribute::AnnotationConfidence, 91u16),
    ];
    assert_eq!(TABLE.len(), 83, "table must grow with the Attribute enum");
    for (name, expected, id) in TABLE {
        let parsed = Attribute::from_name(name)
            .unwrap_or_else(|_| panic!("{name} must parse"));
        assert_eq!(&parsed, expected, "{name} resolves");
        assert_eq!(parsed.id(), *id, "{name} id");
        // Lowercase folds to the same variant (the case-insensitive contract).
        assert_eq!(
            Attribute::from_name(&name.to_ascii_lowercase()).unwrap(),
            *expected,
            "{name} lowercase folds"
        );
    }
}
