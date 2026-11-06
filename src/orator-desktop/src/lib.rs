#![forbid(unsafe_code)]

//! orator-desktop: Linux desktop surface: tray, global hotkeys, notifications, D-Bus service, and text injection.

pub mod clipboard;
pub mod bind;
pub mod continuation;
pub mod dbus;
pub mod hotkey;
pub mod indicators;
pub mod krunner;
pub mod ladder;
pub mod notify;
pub mod refine;
pub mod tray;

pub use clipboard::{
    paste_with_restore, ChordSender, Clipboard, ExternalClipboard, InMemoryClipboard, NoChord,
};
pub use bind::{
    probe_dbus, probe_hotkeys, probe_krunner, probe_notify, probe_tray, read_desktop_env,
    DbusRoute, DesktopEnv, HotkeyRoute, KrunnerRoute, NotifyRoute, TrayRoute,
};
pub use continuation::continuation_prefix;
pub use dbus::{serves as dbus_serves, Caller, SignalPolicy, SERVICE_NAME};
pub use hotkey::{fallback_bindings, parse_chord, Chord};
pub use indicators::{consent_reminder, indicators_mandatory, session_indicator};
pub use krunner::{parse_query, RunnerOffer, RunnerQuery};
pub use ladder::{inject, InjectError, InjectOutcome, LadderMemory, Rung};
pub use refine::{refine_transcript, wrap_transcript, RefineSource, RefinedTranscript, DEFAULT_REFINE_PROMPT};
pub use notify::{Notification, NotifyAction, NotifySink};
pub use tray::{TrayState, TrayStatus};
