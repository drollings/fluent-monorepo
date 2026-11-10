//! Narration level goldens: parsing, sentence slicing, spoken
//! composition, and the fluent-wvr single source. All hermetic.

use crate::narration::{
    build_summary_prompt, compose_summary_spoken, first_sentence_of, initial_narration_mode,
    last_sentence_of, middle_text_of, parse_narration_level, split_sentences, spoken_for_narration,
    NarrationLevel, NarrationSetting,
};
use fluent_wvr::{Describable, FieldAccess, WorkUnit};

#[test]
fn levels_parse_and_round_trip() {
    assert_eq!(parse_narration_level("none"), Ok(NarrationLevel::None));
    assert_eq!(parse_narration_level("intro"), Ok(NarrationLevel::Intro));
    assert_eq!(
        parse_narration_level("summary"),
        Ok(NarrationLevel::Summary)
    );
    assert_eq!(parse_narration_level("full"), Ok(NarrationLevel::Full));
    assert_eq!(parse_narration_level("  full  "), Ok(NarrationLevel::Full));
    assert!(parse_narration_level("loud").is_err());
    assert!(parse_narration_level("").is_err());
    for level in [
        NarrationLevel::None,
        NarrationLevel::Intro,
        NarrationLevel::Summary,
        NarrationLevel::Full,
    ] {
        assert_eq!(parse_narration_level(level.as_str()), Ok(level));
        assert_eq!(level.to_string(), level.as_str());
    }
    assert_eq!(NarrationLevel::ALL, ["none", "intro", "summary", "full"]);
    assert_eq!(NarrationLevel::default(), NarrationLevel::None);
}

#[test]
fn sentences_split_on_terminators() {
    assert_eq!(
        split_sentences("First. Second! Third?"),
        vec![
            "First.".to_string(),
            "Second!".to_string(),
            "Third?".to_string()
        ]
    );
    assert_eq!(
        split_sentences("No terminator here"),
        vec!["No terminator here".to_string()]
    );
    assert!(split_sentences("   ").is_empty());
    assert_eq!(first_sentence_of("First. Second."), "First.");
    assert_eq!(last_sentence_of("First. Second."), "Second.");
    assert_eq!(first_sentence_of("Only one"), "Only one");
    assert_eq!(last_sentence_of("Only one"), "Only one");
    assert_eq!(middle_text_of("First. Second."), "");
    assert_eq!(middle_text_of("Only one"), "");
    assert_eq!(middle_text_of("First. Middle here. Last."), "Middle here.");
}

#[test]
fn spoken_selection_names_its_slice() {
    assert_eq!(spoken_for_narration("hi", NarrationLevel::None), None);
    assert_eq!(
        spoken_for_narration("First. Second.", NarrationLevel::Intro),
        Some("First.".to_string())
    );
    assert_eq!(
        spoken_for_narration("full reply", NarrationLevel::Full),
        Some("full reply".to_string())
    );
    assert_eq!(
        spoken_for_narration("hi", NarrationLevel::Summary),
        None,
        "summary needs the LLM middle first"
    );
    assert_eq!(
        spoken_for_narration("   ", NarrationLevel::Intro),
        None,
        "blank voices nothing"
    );
    assert_eq!(spoken_for_narration("   ", NarrationLevel::Full), None);
}

#[test]
fn summary_speech_composes_first_middle_last() {
    assert_eq!(
        compose_summary_spoken("First.", "Middle summary.", "Last."),
        "First. Middle summary. Last."
    );
    assert_eq!(
        compose_summary_spoken("Only.", "", "Only."),
        "Only.",
        "one sentence voices once"
    );
    assert_eq!(
        compose_summary_spoken("First.", "", "Last."),
        "First. Last."
    );
    assert_eq!(compose_summary_spoken("", "", ""), "");
    let prompt = build_summary_prompt("middle words");
    assert!(prompt.contains("middle words"));
    assert!(prompt.contains("spoken"));
}

#[test]
fn setting_round_trips_through_field_access() {
    let mut setting = NarrationSetting::default();
    assert_eq!(setting.level, "none");
    setting
        .set_field("level", "full")
        .expect("valid level writes");
    assert_eq!(setting.get_field("level").expect("readable"), "full");
    assert!(setting.field_names().contains(&"level"));
    assert_eq!(setting.resolve(), Ok(NarrationLevel::Full));
    let schema = setting.describe();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["level"].is_object());
}

#[test]
fn setting_execute_validates_the_level() {
    use fluent_wvr::WorkContext;
    let good = NarrationSetting::new(NarrationLevel::Intro);
    assert_eq!(good.name(), "narration");
    assert!(good.execute(&WorkContext::default()).is_ok());
    let bad = NarrationSetting {
        level: "loud".to_string(),
    };
    assert!(bad.execute(&WorkContext::default()).is_err());
}

#[test]
fn initial_mode_follows_config_with_loud_fallback() {
    let config = orator::OratorConfig::default();
    assert_eq!(initial_narration_mode(&config), NarrationLevel::None);
    let mut intro = orator::OratorConfig::default();
    intro.chat.narration = "intro".to_string();
    assert_eq!(initial_narration_mode(&intro), NarrationLevel::Intro);
    let mut garbage = orator::OratorConfig::default();
    garbage.chat.narration = "loud".to_string();
    assert_eq!(initial_narration_mode(&garbage), NarrationLevel::None);
}

#[test]
fn chat_slashes_cover_narration_and_set() {
    use crate::chat::{parse_chat_line, ChatLine, ChatSlash};
    assert_eq!(
        parse_chat_line("/narration intro"),
        Some(ChatLine::Slash(ChatSlash::Narration(NarrationLevel::Intro)))
    );
    assert_eq!(
        parse_chat_line("/narration full"),
        Some(ChatLine::Slash(ChatSlash::Narration(NarrationLevel::Full)))
    );
    assert_eq!(
        parse_chat_line("/narration loud"),
        Some(ChatLine::Say("/narration loud".to_string()))
    );
    assert_eq!(
        parse_chat_line("/set narration summary"),
        Some(ChatLine::Slash(ChatSlash::Narration(
            NarrationLevel::Summary
        )))
    );
    assert_eq!(
        parse_chat_line("/set narration=full"),
        Some(ChatLine::Slash(ChatSlash::Narration(NarrationLevel::Full)))
    );
    assert_eq!(
        parse_chat_line("/set narration loud"),
        Some(ChatLine::Say("/set narration loud".to_string()))
    );
    assert_eq!(
        parse_chat_line("/set voice off"),
        Some(ChatLine::Slash(ChatSlash::Voice(
            crate::chat::ChatVoiceMode::Off
        )))
    );
    assert_eq!(
        parse_chat_line("/set bogus value"),
        Some(ChatLine::Say("/set bogus value".to_string()))
    );
}
