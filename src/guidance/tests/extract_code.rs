//! P2 code-extraction tests (ports `test/unit/extraction/code.test.mjs`:
//! metadata/scope/ranges, language symbols, outlines, remap, surrogates).

use crate::extractor::{ChunkOptions, ExtractSource};
use crate::search_types::FileKind;

fn code_source(format: &str, text: &str) -> ExtractSource {
    ExtractSource {
        file_id: "file".to_string(),
        text: text.to_string(),
        format: format.to_string(),
        kind: FileKind::Code,
    }
}

fn extract(format: &str, text: &str, max: usize, overlap: usize) -> Vec<crate::extractor::PreparedFragment> {
    let mut parser = crate::ast_parser::AstParser::new();
    let extractor = crate::extractor::code::CodeExtractor;
    extractor
        .extract_for_indexing(&mut parser, &code_source(format, text), ChunkOptions {
            max_chunk_chars: max,
            chunk_overlap_chars: overlap,
        })
        .expect("extract")
}

fn named<'a>(frags: &'a [crate::extractor::PreparedFragment], name: &str) -> &'a crate::extractor::PreparedFragment {
    frags
        .iter()
        .find(|f| matches!(&f.metadata, Some(crate::extractor::FragmentMetadata::Code { symbol_name: Some(n), .. }) if n == name))
        .unwrap_or_else(|| panic!("no fragment named {name}: {frags:?}"))
}

fn assert_source_backed(source: &ExtractSource, frag: &crate::extractor::PreparedFragment) {
    let slice = &source.text.as_bytes()[frag.range.start_offset..frag.range.end_offset];
    assert_eq!(slice, frag.content_text.as_bytes(), "{frag:?}");
}

#[test]
fn typescript_metadata_scope_and_ranges() {
    let source = code_source(
        "typescript",
        &[
            "/** Adds one. */",
            "async function add(value: number): Promise<number> {",
            "  return helper(value);",
            "}",
            "export function publish() { return add(1); }",
            "class Box {",
            "  private value = 1;",
            "  static create() { return new Box(); }",
            "}",
        ]
        .join("\n"),
    );
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &source, ChunkOptions { max_chunk_chars: 500, chunk_overlap_chars: 50 })
        .expect("extract");
    let add = named(&frags, "add");
    let publish = named(&frags, "publish");
    let create = named(&frags, "create");
    assert_source_backed(&source, add);
    assert_source_backed(&source, publish);
    assert_source_backed(&source, create);
    match &add.metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, symbol_name, scope, node_type, signature, doc, modifiers }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(symbol_name.as_deref(), Some("add"));
            assert!(scope.is_none(), "{scope:?}");
            assert_eq!(node_type, "function_declaration");
            assert_eq!(signature.as_deref(), Some("async function add(value: number): Promise<number>"));
            assert_eq!(doc.as_deref(), Some("Adds one."));
            assert_eq!(modifiers, &["async".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    match &publish.metadata {
        Some(crate::extractor::FragmentMetadata::Code { modifiers, .. }) => {
            assert_eq!(modifiers, &["exported".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    match &create.metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, symbol_name, scope, node_type, signature, doc, modifiers }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(symbol_name.as_deref(), Some("create"));
            assert_eq!(scope.as_deref(), Some("Box"));
            assert_eq!(node_type, "method_definition");
            assert_eq!(signature.as_deref(), Some("static create()"));
            assert!(doc.is_none());
            assert_eq!(modifiers, &["static".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(add.range.start_line, 2);
    assert_eq!(create.range.start_line, 8);
    let mut ids: Vec<&str> = frags.iter().map(|f| f.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), frags.len());
    assert!(frags.iter().all(|f| f.id.len() == 64), "{frags:?}");
}

#[test]
fn language_specific_symbols_and_scopes() {
    let c = extract("c", "typedef struct Widget { int value; } Widget;\nstatic int add(int a, int b) { return a + b; }", 3600, 540);
    let go = extract("go", "package demo\ntype Widget struct { value int }\nfunc (w *Widget) Value() int { return w.value }\ntype Reader interface { Read() string }", 3600, 540);
    let python = extract("python", "class Service:\n    @staticmethod\n    async def fetch(value: str) -> str:\n        return value", 3600, 540);

    match &named(&c, "Widget").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, symbol_name, scope, node_type, signature, doc, modifiers }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Class);
            assert_eq!(symbol_name.as_deref(), Some("Widget"));
            assert!(scope.is_none());
            assert_eq!(node_type, "type_definition");
            assert_eq!(signature.as_deref(), Some("typedef struct Widget { int value; } Widget"));
            assert!(doc.is_none());
            assert!(modifiers.is_empty(), "{modifiers:?}");
        }
        other => panic!("{other:?}"),
    }
    match &named(&c, "add").metadata {
        Some(crate::extractor::FragmentMetadata::Code { modifiers, .. }) => {
            assert_eq!(modifiers, &["static".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    match &named(&go, "Value").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, symbol_name, scope, node_type, signature, modifiers, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(symbol_name.as_deref(), Some("Value"));
            assert_eq!(scope.as_deref(), Some("Widget"));
            assert_eq!(node_type, "method_declaration");
            assert_eq!(signature.as_deref(), Some("func (w *Widget) Value() int"));
            assert_eq!(modifiers, &["exported".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    match &named(&go, "Reader").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Interface);
        }
        other => panic!("{other:?}"),
    }
    match &named(&go, "Read").metadata {
        Some(crate::extractor::FragmentMetadata::Code { scope, .. }) => {
            assert_eq!(scope.as_deref(), Some("Reader"));
        }
        other => panic!("{other:?}"),
    }
    match &named(&python, "fetch").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, symbol_name, scope, signature, modifiers, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(symbol_name.as_deref(), Some("fetch"));
            assert_eq!(scope.as_deref(), Some("Service"));
            assert_eq!(signature.as_deref(), Some("async def fetch(value: str) -> str:"));
            assert_eq!(modifiers, &["async".to_string(), "static".to_string()]);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn large_entities_emit_outlines_and_grouped_windows() {
    let source = code_source(
        "typescript",
        &[
            "export class Service {",
            "  first(value: string) { return value.repeat(20); }",
            "  second() { return this.first(fetchValue()); }",
            "  third() { return new Service(); }",
            "}",
            "export function orchestrate(value: string) {",
            "  const first = load(value);",
            "  const second = client.fetch(first);",
            "  return finalize(second);",
            "}",
        ]
        .join("\n"),
    );
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &source, ChunkOptions { max_chunk_chars: 140, chunk_overlap_chars: 30 })
        .expect("extract");
    let outline = |name: &str| {
        frags
            .iter()
            .find(|f| {
                matches!(&f.metadata, Some(crate::extractor::FragmentMetadata::Code { symbol_name: Some(n), .. }) if n == name)
                    && f.group.as_deref() == Some(f.id.as_str())
            })
            .unwrap_or_else(|| panic!("no outline for {name}: {frags:?}"))
    };
    let service = outline("Service");
    let orchestrate = outline("orchestrate");
    assert!(service.content_text.contains("members:"), "{:?}", service.content_text);
    assert!(service.content_text.contains("function first(value: string)"), "{:?}", service.content_text);
    assert!(service.content_text.contains("function second()"), "{:?}", service.content_text);
    assert!(orchestrate.content_text.contains("calls: load, client.fetch, finalize"), "{:?}", orchestrate.content_text);

    let windows: Vec<_> = frags
        .iter()
        .filter(|f| f.group.as_deref() == Some(service.id.as_str()) && f.id != service.id)
        .collect();
    assert!(windows.len() >= 2, "{frags:?}");
    for window in &windows {
        assert_source_backed(&source, window);
        assert!(crate::extractor::utf16_len(&window.content_text) <= 140, "{window:?}");
    }
    assert!(
        windows[0].range.end_offset > windows[1].range.start_offset,
        "adjacent windows should preserve the requested overlap"
    );
}

#[test]
fn gaps_compact_for_embedding_without_changing_windows() {
    let mut text = vec!["def spaced() -> str:".to_string(), "    first_value = prepare()".to_string()];
    text.extend(std::iter::repeat_n(String::new(), 70));
    text.push("    second_value = transform(first_value)".to_string());
    text.push("    return second_value".to_string());
    let source = code_source("python", &text.join("\n"));
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &source, ChunkOptions { max_chunk_chars: 120, chunk_overlap_chars: 18 })
        .expect("extract");
    let major = frags
        .iter()
        .find(|f| {
            matches!(&f.metadata, Some(crate::extractor::FragmentMetadata::Code { symbol_name: Some(n), .. }) if n == "spaced")
                && f.group.as_deref() == Some(f.id.as_str())
        })
        .expect("outline major");
    assert!(major.content_text.contains("calls: prepare, transform"), "{:?}", major.content_text);
    let window = frags
        .iter()
        .find(|f| {
            f.group.as_deref() == Some(major.id.as_str())
                && f.embedding_text.as_deref().is_some_and(|e| {
                    e.contains("first_value = prepare()") && e.contains("second_value = transform(first_value)")
                })
        })
        .expect("compact window");
    assert_source_backed(&source, window);
    assert!(window.content_text.len() > 120);
    assert!(!window.embedding_text.as_deref().unwrap().contains("\n\n"));
}

#[test]
fn script_blocks_remap_and_fall_back() {
    let source = code_source(
        "svelte",
        &[
            "<h1>Hello</h1>",
            "<script>",
            "export const first = () => 1;",
            "</script>",
            "<p>Middle</p>",
            "<script lang=\"ts\">",
            "export function second(value: number) { return value; }",
            "</script>",
        ]
        .join("\n"),
    );
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &source, ChunkOptions { max_chunk_chars: 3600, chunk_overlap_chars: 540 })
        .expect("extract");
    let first = named(&frags, "first");
    let second = named(&frags, "second");
    assert_source_backed(&source, first);
    assert_source_backed(&source, second);
    assert_eq!(first.range.start_line, 3);
    assert_eq!(second.range.start_line, 7);

    let plain = code_source("vue", "<template><p>x</p></template>\n<script lang=\"ts\">\n// no declarations\n</script>");
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &plain, ChunkOptions { max_chunk_chars: 3600, chunk_overlap_chars: 540 })
        .expect("extract");
    assert_eq!(frags.len(), 1);
    assert_source_backed(&plain, &frags[0]);
    assert!(frags[0].metadata.is_none());
    assert_eq!(frags[0].content_text, "\n// no declarations\n");

    let no_script = code_source("svelte", "<h1>No script</h1>");
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &no_script, ChunkOptions { max_chunk_chars: 3600, chunk_overlap_chars: 540 })
        .expect("extract");
    assert_eq!(frags.len(), 1);
    assert!(frags[0].metadata.is_none());
    assert_eq!(frags[0].content_text, "<h1>No script</h1>");
}

#[test]
fn rust_struct_impl_and_functions() {
    let frags = extract(
        "rust",
        "/// A widget.\n///\n/// Details.\npub struct Widget {\n    pub value: i32,\n}\n\nimpl Widget {\n    /// Create one.\n    pub fn create(value: i32) -> Self {\n        Widget { value }\n    }\n}\n\nfn helper() {}",
        3600,
        540,
    );
    let widget = named(&frags, "Widget");
    match &widget.metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, scope, node_type, doc, modifiers, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Class);
            assert!(scope.is_none(), "{scope:?}");
            assert_eq!(node_type, "struct_item");
            assert_eq!(doc.as_deref(), Some("A widget.\n\nDetails."));
            assert_eq!(modifiers, &["public".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    let create = named(&frags, "create");
    match &create.metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, scope, signature, doc, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(scope.as_deref(), Some("Widget"));
            assert_eq!(signature.as_deref(), Some("pub fn create(value: i32) -> Self"));
            assert_eq!(doc.as_deref(), Some("Create one."));
        }
        other => panic!("{other:?}"),
    }
    assert!(frags.iter().any(|f| matches!(&f.metadata, Some(crate::extractor::FragmentMetadata::Code { symbol_name: Some(n), .. }) if n == "helper")));
}

#[test]
fn java_and_cpp_smoke() {
    let java = extract(
        "java",
        "package demo;\n/** Makes things. */\npublic class Maker {\n    private int count;\n    public static Maker create() { return new Maker(); }\n}",
        3600,
        540,
    );
    match &named(&java, "Maker").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, doc, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Class);
            assert_eq!(doc.as_deref(), Some("Makes things."));
        }
        other => panic!("{other:?}"),
    }
    match &named(&java, "create").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, scope, modifiers, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Function);
            assert_eq!(scope.as_deref(), Some("Maker"));
            assert!(modifiers.contains(&"public".to_string()), "{modifiers:?}");
            assert!(modifiers.contains(&"static".to_string()), "{modifiers:?}");
        }
        other => panic!("{other:?}"),
    }
    let cpp = extract(
        "cpp",
        "namespace demo {\nclass Widget {\n public:\n  int value() { return 1; }\n};\n}",
        3600,
        540,
    );
    match &named(&cpp, "Widget").metadata {
        Some(crate::extractor::FragmentMetadata::Code { symbol_type, .. }) => {
            assert_eq!(*symbol_type, crate::search_types::CodeSymbolType::Class);
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn chunk_boundaries_never_split_characters() {
    let emoji = "😀".repeat(80);
    let source = code_source("typescript", &format!("export function emoji() {{ return \"{emoji}\"; }}"));
    let mut parser = crate::ast_parser::AstParser::new();
    let frags = crate::extractor::code::CodeExtractor
        .extract_for_indexing(&mut parser, &source, ChunkOptions { max_chunk_chars: 31, chunk_overlap_chars: 7 })
        .expect("extract");
    let windows: Vec<_> = frags
        .iter()
        .filter(|f| {
            matches!(&f.metadata, Some(crate::extractor::FragmentMetadata::Code { symbol_name: Some(n), .. }) if n == "emoji")
                && f.group.as_deref() != Some(f.id.as_str())
        })
        .collect();
    assert!(windows.len() > 2, "{frags:?}");
    for window in &windows {
        assert_source_backed(&source, window);
        assert!(crate::extractor::utf16_len(&window.content_text) <= 31, "{window:?}");
        assert!(window.content_text.is_char_boundary(window.content_text.len()));
    }
}
