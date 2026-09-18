//! Golden outputs for [`common_core::string::ascii_alnum_tokens`] and its
//! streaming iterator. These vectors lock the ASCII-alphanumeric,
//! case-preserving segmentation contract: any refactor of the segmentation
//! body must keep every vector byte-identical.

use common_core::string::{ascii_alnum_tokens, AsciiAlnumTokens};

const CORPUS: &[&str] = &[
    "",
    "___",
    "x",
    "bug_triage",
    "Foo.Bar-baz",
    "a  b",
    "hello, world! test 123",
    "naïve",
    "café-au-lait 42",
    "emoji 😀 split",
    "tabs\tand\nnewlines\r\n",
    "___leading...trailing___",
    "MiXeD123_Case456",
    "a/b\\c:d;e,f.g!h?i@j#k$l%m^n&o*p(q)r[s]t{u}v|w~x`y\"z",
    "0123456789",
    "long token abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789 end",
];

#[test]
fn ascii_alnum_tokens_goldens() {
    let expected: &[Vec<&str>] = &[
        vec![],
        vec![],
        vec!["x"],
        vec!["bug", "triage"],
        vec!["Foo", "Bar", "baz"],
        vec!["a", "b"],
        vec!["hello", "world", "test", "123"],
        vec!["na", "ve"],
        vec!["caf", "au", "lait", "42"],
        vec!["emoji", "split"],
        vec!["tabs", "and", "newlines"],
        vec!["leading", "trailing"],
        vec!["MiXeD123", "Case456"],
        vec![
            "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q",
            "r", "s", "t", "u", "v", "w", "x", "y", "z",
        ],
        vec!["0123456789"],
        vec![
            "long",
            "token",
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            "end",
        ],
    ];
    assert_eq!(CORPUS.len(), expected.len());
    for (input, want) in CORPUS.iter().zip(expected.iter()) {
        assert_eq!(&ascii_alnum_tokens(input), want, "input: {input:?}");
    }
}

#[test]
fn iterator_agrees_with_owned_fn_on_corpus() {
    for input in CORPUS {
        let via_iter: Vec<String> = AsciiAlnumTokens::new(input).map(str::to_string).collect();
        assert_eq!(&via_iter, &ascii_alnum_tokens(input), "input: {input:?}");
    }
}
