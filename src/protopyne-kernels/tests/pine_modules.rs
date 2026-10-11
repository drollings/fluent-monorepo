//! Import graph tests: the real `_FastMAs -> {libMAs, libUtil}` triple plus
//! synthetic refusal cases.
//!
//! Resolution is pure over supplied values (no I/O inside): each test loads
//! or synthesizes its modules, then asserts order, alias binding, export
//! visibility, and refusal reasons.

use super::*;
use crate::{load_pine_source, parse_pine_source, parse_program};
use std::path::PathBuf;

fn data_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../data/protopyne-rs")
        .join(name)
}

fn library_id(publisher: &str, name: &str, version: u64) -> LibraryId {
    LibraryId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
        version,
    }
}

/// Load one real file as a graph input: identity from the header scan,
/// structure from the parser, both over the same byte-identical text.
fn real_module(id: LibraryId, file: &str) -> ModuleInput {
    let path = data_path(file);
    let source = load_pine_source(&path).expect("target loads");
    let program = parse_program(&source.text).expect("target parses");
    ModuleInput::new(id, source, program)
}

fn real_triple() -> Vec<ModuleInput> {
    vec![
        real_module(library_id("showmethegrail", "libUtil", 12), "libraries/_libUtil.pine"),
        real_module(library_id("showmethegrail", "libMAs", 95), "libraries/_libMAs.pine"),
        // The indicator is the graph root: it declares no library identity
        // of its own, so it travels under its file stem.
        real_module(
            library_id("local", "FastMAs", 6),
            "indicators/_FastMAs.pine",
        ),
    ]
}

/// Synthesize one library module from inline text (pure, no files).
fn synthetic_module(publisher: &str, name: &str, text: &str) -> ModuleInput {
    let source = parse_pine_source(&format!("{publisher}/{name}"), text);
    let program = parse_program(text).expect("synthetic parses");
    ModuleInput::new(library_id(publisher, name, 1), source, program)
}

fn module_key(publisher: &str, name: &str) -> ModuleId {
    ModuleId {
        publisher: publisher.to_owned(),
        name: name.to_owned(),
    }
}

#[test]
fn real_triple_resolves_dependencies_first() {
    let graph = resolve_imports(real_triple()).expect("triple resolves");
    let order = graph.order();
    assert_eq!(order.len(), 3, "all three modules ordered");
    let position = |publisher: &str, name: &str| {
        order
            .iter()
            .position(|id| id == &module_key(publisher, name))
            .expect("module present")
    };
    assert!(
        position("showmethegrail", "libUtil") < position("showmethegrail", "libMAs"),
        "transitive libUtil precedes libMAs: {order:?}"
    );
    assert!(
        position("showmethegrail", "libMAs") < position("local", "FastMAs"),
        "libMAs precedes the indicator root: {order:?}"
    );
    assert!(graph.warnings().is_empty(), "clean v6 triple warns nothing");
}

#[test]
fn real_triple_aliases_bind_to_exports() {
    let graph = resolve_imports(real_triple()).expect("triple resolves");
    let root = module_key("local", "FastMAs");
    let aliases = graph.alias_table(&root).expect("root aliases known");
    assert_eq!(aliases.get("UTIL"), Some(&module_key("showmethegrail", "libUtil")));
    assert_eq!(aliases.get("libMAs"), Some(&module_key("showmethegrail", "libMAs")));

    let util = graph
        .resolve_reference(&root, "UTIL", "Smoothings")
        .expect("UTIL.Smoothings binds");
    assert_eq!(util, &module_key("showmethegrail", "libUtil"));
    let factory = graph
        .resolve_reference(&root, "libMAs", "factorySmoothing")
        .expect("libMAs.factorySmoothing binds");
    assert_eq!(factory, &module_key("showmethegrail", "libMAs"));

    // Transitive leg: libMAs sees libUtil under its own alias.
    let mas = module_key("showmethegrail", "libMAs");
    let transitive = graph
        .resolve_reference(&mas, "UTIL", "Smoothings")
        .expect("transitive UTIL.Smoothings binds");
    assert_eq!(transitive, &module_key("showmethegrail", "libUtil"));
}

#[test]
fn unknown_import_refuses_with_reason() {
    let modules = vec![synthetic_module(
        "local",
        "lonely",
        "import nobodypub/nowhere/1 as XX\nlonely() =>\n    XX.help()",
    )];
    let err = resolve_imports(modules).expect_err("unknown import refuses");
    let reason = err.to_string();
    assert!(
        reason.contains("nobodypub/nowhere"),
        "reason names the unknown module: {reason}"
    );
    assert!(
        reason.contains("lonely"),
        "reason names the importer: {reason}"
    );
}

#[test]
fn import_cycle_refuses_with_reason() {
    let modules = vec![
        synthetic_module("local", "alpha", "import local/beta/1 as B\nalpha() =>\n    B.beta()"),
        synthetic_module("local", "beta", "import local/alpha/1 as A\nalpha2() =>\n    A.alpha()"),
    ];
    let err = resolve_imports(modules).expect_err("cycle refuses");
    let reason = err.to_string();
    assert!(reason.contains("cycle"), "reason names the cycle: {reason}");
}

#[test]
fn version_five_double_load_warns_once_and_resolves() {
    let text_v5 = "//@version=5\nlibrary('libUtil')\nhelper() =>\n    1";
    let first = synthetic_module("showmethegrail", "libUtil", text_v5);
    let second = synthetic_module("showmethegrail", "libUtil", text_v5);
    let graph = resolve_imports(vec![first, second]).expect("warn-only version still resolves");
    assert_eq!(graph.order().len(), 1, "duplicate loads dedup to one module");
    assert_eq!(
        graph.warnings().len(),
        1,
        "version warning fires once: {:?}",
        graph.warnings()
    );
    assert!(
        graph.warnings()[0].message.contains('5'),
        "warning names the declared version"
    );
}

#[test]
fn non_exported_helper_is_invisible_to_importers() {
    let modules = vec![
        synthetic_module(
            "local",
            "lib",
            "export visible() =>\n    1\nhelper() =>\n    2",
        ),
        synthetic_module("local", "user", "import local/lib/1 as LIB\nrun() =>\n    LIB.visible()"),
    ];
    let graph = resolve_imports(modules).expect("pair resolves");
    let user = module_key("local", "user");
    graph
        .resolve_reference(&user, "LIB", "visible")
        .expect("exported name binds");
    let err = graph
        .resolve_reference(&user, "LIB", "helper")
        .expect_err("bare helper is not importable");
    let reason = err.to_string();
    assert!(
        reason.contains("helper") && reason.contains("not exported"),
        "reason names the hidden item: {reason}"
    );
    // Unknown alias is a separate refusal, not a false bind.
    let alias_err = graph
        .resolve_reference(&user, "NOPE", "visible")
        .expect_err("unknown alias refuses");
    assert!(
        alias_err.to_string().contains("NOPE"),
        "reason names the unknown alias"
    );
}
