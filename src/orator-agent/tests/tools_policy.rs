//! Policy tests: tier table, ambient deny, session deny, taint ask,
//! and the locked T2 toggle.

use fluent_wvr::FieldAccess;
use orator::SessionKind;

use crate::tools::policy::{CallOrigin, Policy, PolicyCtx, Verdict};
use crate::tools::tool::{ToolCall, ToolTier};

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
        policy.decide(
            &call("time_now"),
            ToolTier::T0,
            &ctx(SessionKind::Assistant)
        ),
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
    policy
        .set_field("disabled_tools", "shell, file_write")
        .expect("csv sets");
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
    assert_eq!(
        policy.get_field("t2_always_ask").expect("read back"),
        "true"
    );
}

/// M15-S1: the policy field surface. `describe()` is the schema
/// golden; the table pins the custom semantics (comma-split list,
/// locked-on toggle) and reject paths.
#[test]
fn policy_field_surface_pins_schema_and_table() {
    use fluent_wvr::Describable;
    let mut policy = Policy::new();
    assert_eq!(
        policy.describe(),
        serde_json::json!({
            "type": "object",
            "properties": {
                "t1_require_verified": { "type": "boolean", "description": "T1 needs a verified speaker" },
                "disabled_tools": { "type": "string", "description": "Comma-separated disabled tool names" },
                "t2_always_ask": { "type": "boolean", "description": "T2 always asks (locked on)" }
            },
            "required": ["t1_require_verified", "disabled_tools", "t2_always_ask"]
        })
    );
    assert_eq!(
        policy.field_names(),
        &["t1_require_verified", "disabled_tools", "t2_always_ask"]
    );
    policy
        .set_field("t1_require_verified", "false")
        .expect("bool sets");
    assert_eq!(
        policy.get_field("t1_require_verified").expect("reads"),
        "false"
    );
    policy
        .set_field("t1_require_verified", "maybe")
        .expect_err("garbage bool rejects");
    policy
        .set_field("disabled_tools", "a,,b")
        .expect("csv sets");
    assert_eq!(policy.get_field("disabled_tools").expect("reads"), "a,b");
    policy.set_field("nope", "1").expect_err("unknown rejects");
    policy.get_field("nope").expect_err("unknown reads reject");
}
