//! PTT goldens: parsing, session starts, and the fluent-wvr single
//! source. All hermetic.

use crate::ptt::{initial_ptt_mode, parse_ptt_mode, PttMode, PttSetting};
use fluent_wvr::{Describable, FieldAccess, WorkUnit};

#[test]
fn levels_parse_and_round_trip() {
    assert_eq!(parse_ptt_mode("off"), Ok(PttMode::Off));
    assert_eq!(parse_ptt_mode("on"), Ok(PttMode::On));
    assert_eq!(parse_ptt_mode("  on  "), Ok(PttMode::On));
    assert!(parse_ptt_mode("toggle").is_err());
    assert!(parse_ptt_mode("").is_err());
    for level in [PttMode::Off, PttMode::On] {
        assert_eq!(parse_ptt_mode(level.as_str()), Ok(level));
        assert_eq!(level.to_string(), level.as_str());
    }
    assert_eq!(PttMode::ALL, ["off", "on"]);
    assert_eq!(PttMode::default(), PttMode::Off);
}

#[test]
fn setting_round_trips_through_field_access() {
    let mut setting = PttSetting::default();
    assert_eq!(setting.level, "off");
    setting
        .set_field("level", "on")
        .expect("valid level writes");
    assert_eq!(setting.get_field("level").expect("readable"), "on");
    assert!(setting.field_names().contains(&"level"));
    assert_eq!(setting.resolve(), Ok(PttMode::On));
    let schema = setting.describe();
    assert_eq!(schema["type"], "object");
    assert!(schema["properties"]["level"].is_object());
}

#[test]
fn setting_execute_validates_the_level() {
    use fluent_wvr::WorkContext;
    let good = PttSetting::new(PttMode::On);
    assert_eq!(good.name(), "ptt");
    assert!(good.execute(&WorkContext::default()).is_ok());
    let bad = PttSetting {
        level: "loud".to_string(),
    };
    assert!(bad.execute(&WorkContext::default()).is_err());
}

#[test]
fn initial_mode_follows_config_with_loud_fallback() {
    let config = orator::OratorConfig::default();
    assert_eq!(initial_ptt_mode(&config), PttMode::Off);
    let mut on = orator::OratorConfig::default();
    on.chat.ptt = "on".to_string();
    assert_eq!(initial_ptt_mode(&on), PttMode::On);
    let mut garbage = orator::OratorConfig::default();
    garbage.chat.ptt = "loud".to_string();
    assert_eq!(initial_ptt_mode(&garbage), PttMode::Off);
}

#[test]
fn chat_slashes_cover_ptt_and_set() {
    use crate::chat::{parse_chat_line, ChatLine, ChatSlash};
    assert_eq!(
        parse_chat_line("/ptt on"),
        Some(ChatLine::Slash(ChatSlash::Ptt(PttMode::On)))
    );
    assert_eq!(
        parse_chat_line("/ptt off"),
        Some(ChatLine::Slash(ChatSlash::Ptt(PttMode::Off)))
    );
    assert_eq!(
        parse_chat_line("/ptt loudly"),
        Some(ChatLine::Say("/ptt loudly".to_string()))
    );
    assert_eq!(
        parse_chat_line("/set ptt on"),
        Some(ChatLine::Slash(ChatSlash::Ptt(PttMode::On)))
    );
    assert_eq!(
        parse_chat_line("/set ptt=off"),
        Some(ChatLine::Slash(ChatSlash::Ptt(PttMode::Off)))
    );
    assert_eq!(
        parse_chat_line("/set ptt loudly"),
        Some(ChatLine::Say("/set ptt loudly".to_string()))
    );
}

#[test]
fn ptt_and_mic_modes_track_each_other() {
    use crate::chat::{mic_for_ptt, ptt_for_mic};
    use crate::tui::MicMode;
    assert_eq!(mic_for_ptt(PttMode::Off), MicMode::Auto);
    assert_eq!(mic_for_ptt(PttMode::On), MicMode::PushToTalk);
    assert_eq!(ptt_for_mic(MicMode::Auto), PttMode::Off);
    assert_eq!(ptt_for_mic(MicMode::PushToTalk), PttMode::On);
}
