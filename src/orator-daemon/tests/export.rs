//! Meeting export tests: a scripted session exports exact bytes, unknown
//! ids refuse loudly, re-runs and concurrent runs are byte-identical.
//! Hermetic: temp HOME, scripted turns, no inference, no network.

use crate::voice::{VoiceError, VoiceOwner};

/// Process env is shared across test threads: every export test moves
/// HOME, so they serialize on this lock. The voice boot tests in
/// `tests/voice.rs` mutate HOME/PATH for candidate fixtures and hold
/// this lock too (see `super::export_tests::HOME_LOCK` use there).
pub(crate) static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct TempHome {
    dir: tempfile::TempDir,
    prior_home: Option<std::ffi::OsString>,
    prior_data_home: Option<std::ffi::OsString>,
}

impl TempHome {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let prior_home = std::env::var_os("HOME");
        let prior_data_home = std::env::var_os("XDG_DATA_HOME");
        std::env::set_var("HOME", dir.path());
        std::env::remove_var("XDG_DATA_HOME");
        Self {
            dir,
            prior_home,
            prior_data_home,
        }
    }

    fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }
}

impl Drop for TempHome {
    fn drop(&mut self) {
        match &self.prior_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        match &self.prior_data_home {
            Some(value) => std::env::set_var("XDG_DATA_HOME", value),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
    }
}

fn scripted_owner(id_turns: &[(&str, &str)]) -> (TempHome, VoiceOwner, String) {
    let home = TempHome::new();
    let owner = VoiceOwner::disabled();
    let id = owner.start_session("meeting").expect("meeting starts");
    for (question, reply) in id_turns {
        owner.record_turn_for_tests(&id, question, reply);
    }
    (home, owner, id)
}

#[test]
fn scripted_session_exports_exact_bytes() {
    let _guard = common_core::sync::lock(&HOME_LOCK);
    let (home, owner, id) = scripted_owner(&[("hello there", "hi, ready to take notes")]);
    let dest = owner.export_meeting(&id).expect("export serves");
    assert!(
        dest.starts_with(home.dir()),
        "artifact lands under the temp HOME: {}",
        dest.display()
    );
    assert_eq!(dest.extension().and_then(|ext| ext.to_str()), Some("md"));
    let text = std::fs::read_to_string(&dest).expect("artifact reads");
    assert_eq!(
        text,
        format!(
            "# meeting {id}\n\
             \n\
             turns: 2\n\
             \n\
             ## record\n\
             1. [note] session {id} started (meeting)\n\
             2. [transcript] hello there\n\
             3. [reply] hi, ready to take notes\n\
             \n\
             ## summary\n\
             hello there\n\
             …\n\
             hi, ready to take notes\n"
        )
    );
    owner.shutdown();
}

#[test]
fn unknown_and_unsafe_ids_refuse_loudly() {
    let _guard = common_core::sync::lock(&HOME_LOCK);
    let (_home, owner, _id) = scripted_owner(&[]);
    for bad in ["s999", "", "  ", "../evil", "s1.md", "s/1"] {
        let error = owner.export_meeting(bad).expect_err("unknown refuses");
        assert!(
            matches!(error, VoiceError::UnknownSession(_)),
            "{bad:?} refuses as unknown session: {error:?}"
        );
        assert!(error.to_string().contains("session"), "{error}");
    }
    owner.shutdown();
}

#[test]
fn export_is_idempotent_across_reruns() {
    let _guard = common_core::sync::lock(&HOME_LOCK);
    let (_home, owner, id) = scripted_owner(&[("one", "two")]);
    let first = owner.export_meeting(&id).expect("first export");
    let second = owner.export_meeting(&id).expect("second export");
    assert_eq!(first, second, "same session maps to one path");
    assert_eq!(
        std::fs::read(&first).expect("reads"),
        std::fs::read(&second).expect("reads"),
        "re-running reproduces identical bytes"
    );
    owner.shutdown();
}

#[test]
fn concurrent_exports_agree_byte_for_byte() {
    let _guard = common_core::sync::lock(&HOME_LOCK);
    let (_home, owner, id) = scripted_owner(&[("one", "two")]);
    let owner = std::sync::Arc::new(owner);
    let mut handles = Vec::new();
    for _ in 0..4 {
        let (owner, id) = (std::sync::Arc::clone(&owner), id.clone());
        handles.push(std::thread::spawn(move || owner.export_meeting(&id)));
    }
    let mut texts = Vec::new();
    for handle in handles {
        let dest = handle.join().expect("thread joins").expect("export serves");
        texts.push(std::fs::read(&dest).expect("artifact reads"));
    }
    assert!(
        texts.windows(2).all(|pair| pair[0] == pair[1]),
        "concurrent exports agree"
    );
}

#[test]
fn export_meeting_route_serves_over_both_param_shapes() {
    let _guard = common_core::sync::lock(&HOME_LOCK);
    let (_home, owner, id) = scripted_owner(&[("hello", "hi")]);
    // The `session` shape and the `id` alias serve the same artifact.
    let (ok, session_body) = crate::socket::route_for_tests(
        &owner,
        "ExportMeeting",
        &serde_json::json!({"session": id}),
    );
    assert!(ok, "session shape serves: {session_body}");
    let (ok, id_body) =
        crate::socket::route_for_tests(&owner, "ExportMeeting", &serde_json::json!({"id": id}));
    assert!(ok, "aliased id serves: {id_body}");
    assert_eq!(
        session_body.get("path"),
        id_body.get("path"),
        "both shapes name one artifact"
    );
    assert!(
        id_body
            .get("path")
            .is_some_and(|path| path.as_str().is_some_and(|text| text.ends_with(".md"))),
        "{id_body}"
    );
    owner.shutdown();
}
