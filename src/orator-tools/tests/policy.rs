//! Policy tests: tier table, ambient deny, session deny, taint ask,
//! and the locked T2 toggle.

use fluent_wvr::FieldAccess;
use orator_turn::SessionKind;

use crate::policy::{CallOrigin, Policy, PolicyCtx, Verdict};
use crate::tool::{ToolCall, ToolTier};

fn call(name: &str) -> ToolCall {
    ToolCall {
        name: name.to_string(),
        args: serde_json::json!({}),
        preamble: String::new(),
    }
}

fn ctx(session: SessionKind) -> PolicyCtx {
    PolicyCtx {
        session,
        verified: false,
        tainted: false,
        origin: CallOrigin::Direct,
    }
}

#[test]
fn tier_table_decides() {
    let policy = Policy::new();
    assert_eq!(
        policy.decide(&call("time_now"), ToolTier::T0, &ctx(SessionKind::Assistant)),
        Verdict::Allow
    );
    assert!(matches!(
        policy.decide(&call("notify"), ToolTier::T1, &ctx(SessionKind::Assistant)),
        Verdict::Ask(_)
    ));
    let mut verified = ctx(SessionKind::Assistant);
    verified.verified = true;
    assert_eq!(
        policy.decide(&call("notify"), ToolTier::T1, &verified),
        Verdict::Allow
    );
    assert!(matches!(
        policy.decide(&call("shell"), ToolTier::T2, &verified),
        Verdict::Ask(_)
    ));
}

#[test]
fn ambient_and_untrusted_sessions_deny() {
    let policy = Policy::new();
    let mut ambient = ctx(SessionKind::Assistant);
    ambient.origin = CallOrigin::Ambient;
    assert!(matches!(
        policy.decide(&call("time_now"), ToolTier::T0, &ambient),
        Verdict::Deny(_)
    ));
    for session in [SessionKind::ListenIn, SessionKind::Meeting] {
        assert!(matches!(
            policy.decide(&call("time_now"), ToolTier::T0, &ctx(session)),
            Verdict::Deny(_)
        ));
    }
}

#[test]
fn tainted_turns_ask_for_t2() {
    let policy = Policy::new();
    let mut tainted = ctx(SessionKind::Assistant);
    tainted.verified = true;
    tainted.tainted = true;
    assert!(matches!(
        policy.decide(&call("shell"), ToolTier::T2, &tainted),
        Verdict::Ask(_)
    ));
}

#[test]
fn disabled_tools_deny_and_t2_lock_holds() {
    let mut policy = Policy::new();
    policy.set_field("disabled_tools", "shell, file_write").expect("csv sets");
    assert_eq!(
        policy.get_field("disabled_tools").expect("read back"),
        "shell,file_write"
    );
    assert!(matches!(
        policy.decide(&call("shell"), ToolTier::T2, &ctx(SessionKind::Assistant)),
        Verdict::Deny(_)
    ));
    policy
        .set_field("t2_always_ask", "false")
        .expect_err("no persistent grants in v1");
    assert_eq!(policy.get_field("t2_always_ask").expect("read back"), "true");
}
