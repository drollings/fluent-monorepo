use common_core::cite::*;

#[test]
fn extract_two_spans() {
    let text = "src/foo.rs:42 and src/bar.rs:7";
    let cites = extract_citations(text);
    assert_eq!(cites.len(), 2);
    assert_eq!(cites[0].file, "src/foo.rs");
    assert_eq!(cites[0].line, 42);
    assert_eq!(cites[1].file, "src/bar.rs");
    assert_eq!(cites[1].line, 7);
    // spans should cover the substrings
    assert_eq!(&text[cites[0].span.0..cites[0].span.1], "src/foo.rs:42");
    assert_eq!(&text[cites[1].span.0..cites[1].span.1], "src/bar.rs:7");
}

#[test]
fn extract_citation_at_inside_outside() {
    let text = "see src/foo.rs:42 here";
    let cites = extract_citations(text);
    assert_eq!(cites.len(), 1);
    let span = cites[0].span;
    let inside = span.0 + 2;
    let outside = 0;
    assert_eq!(
        extract_citation_at(text, inside).unwrap().file,
        "src/foo.rs"
    );
    assert!(extract_citation_at(text, outside).is_none());
    // offset just past the span is outside
    assert!(extract_citation_at(text, span.1).is_none());
}

#[test]
fn no_file_extension_ignored() {
    let text = "See Makefile:42";
    let cites = extract_citations(text);
    assert!(cites.is_empty());
    assert!(extract_citation_ranges(text).is_empty());
}

#[test]
fn ranges_cover_start_end_spans() {
    let text = "see src/foo.rs:10-20 and src/bar.rs:7 here";
    let ranges = extract_citation_ranges(text);
    assert_eq!(ranges.len(), 2, "{ranges:?}");
    assert_eq!(ranges[0].file, "src/foo.rs");
    assert_eq!((ranges[0].start, ranges[0].end), (10, 20));
    assert_eq!(
        &text[ranges[0].span.0..ranges[0].span.1],
        "src/foo.rs:10-20"
    );
    assert_eq!(ranges[1].file, "src/bar.rs");
    assert_eq!((ranges[1].start, ranges[1].end), (7, 7));
    assert_eq!(&text[ranges[1].span.0..ranges[1].span.1], "src/bar.rs:7");
}

#[test]
fn ranges_agree_with_line_parser_on_plain_inputs() {
    // Same file-token scan: for `file:line` inputs every range
    // matches its line citation byte-for-byte.
    let inputs = [
        "src/foo.rs:42",
        "see src/a.rs:1 and src/b.rs:9 here",
        "no citations at all",
        "See Makefile:42",
        "src/foo.rs:3-5",
        "trailing src/foo.rs:8-",
    ];
    for text in inputs {
        let lines = extract_citations(text);
        let ranges = extract_citation_ranges(text);
        assert_eq!(lines.len(), ranges.len(), "count agrees: {text:?}");
        for (line, range) in lines.iter().zip(ranges.iter()) {
            assert_eq!(line.file, range.file, "{text:?}");
            assert_eq!(line.line, range.start, "line is the range start: {text:?}");
            if range.start == range.end {
                assert_eq!(line.span, range.span, "span agrees: {text:?}");
            } else {
                assert!(
                    range.span.0 == line.span.0 && range.span.1 > line.span.1,
                    "range span extends the line span: {text:?}"
                );
                assert_eq!(
                    &text[line.span.0..line.span.1],
                    format!("{}:{}", range.file, range.start),
                    "{text:?}"
                );
            }
        }
    }
}
