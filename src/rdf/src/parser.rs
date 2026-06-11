use std::collections::{HashMap, VecDeque};

use crate::lexer::{Lexer, Token, TokenKind};
use crate::{RdfError, RDF_TYPE};

#[derive(Debug, Clone, PartialEq)]
pub struct Literal {
    pub value: String,
    pub lang: Option<String>,
    pub datatype: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    Iri(String),
    BlankNode(String),
    Literal(Literal),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Triple {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
}

pub struct Parser<'a> {
    lex: Lexer<'a>,
    peeked: Option<Token<'a>>,
    prefix_map: HashMap<String, String>,
    base: Option<String>,
    blank_counter: u64,
    queue: VecDeque<Triple>,
}

impl<'a> Parser<'a> {
    pub fn new(src: &'a str) -> Self {
        Self {
            lex: Lexer::new(src),
            peeked: None,
            prefix_map: HashMap::new(),
            base: None,
            blank_counter: 0,
            queue: VecDeque::new(),
        }
    }

    fn peek_tok(&mut self) -> Result<Token<'a>, RdfError> {
        if self.peeked.is_none() {
            self.peeked = Some(self.lex.next_token()?);
        }
        Ok(self.peeked.unwrap())
    }

    fn consume_tok(&mut self) -> Result<Token<'a>, RdfError> {
        if let Some(tok) = self.peeked.take() {
            Ok(tok)
        } else {
            self.lex.next_token()
        }
    }

    fn expect_tok(&mut self, kind: TokenKind) -> Result<Token<'a>, RdfError> {
        let tok = self.consume_tok()?;
        if tok.kind != kind {
            return Err(RdfError::UnexpectedToken {
                line: tok.line,
                col: tok.col,
                expected: format!("{kind:?}"),
                got: format!("{:?}", tok.kind),
            });
        }
        Ok(tok)
    }

    pub fn next_triple(&mut self) -> Result<Option<Triple>, RdfError> {
        loop {
            if let Some(triple) = self.queue.pop_front() {
                return Ok(Some(triple));
            }

            let tok = self.peek_tok()?;
            match tok.kind {
                TokenKind::Eof => return Ok(None),
                TokenKind::Keyword => {
                    let kw = self.consume_tok()?;
                    match kw.value {
                        "@prefix" | "PREFIX" => self.parse_prefix()?,
                        "@base" | "BASE" => self.parse_base()?,
                        _ => {
                            return Err(RdfError::UnexpectedToken {
                                line: kw.line,
                                col: kw.col,
                                expected: "directive".into(),
                                got: kw.value.into(),
                            });
                        }
                    }
                }
                _ => {
                    self.parse_statement()?;
                }
            }
        }
    }
}

impl Iterator for Parser<'_> {
    type Item = Result<Triple, RdfError>;

    fn next(&mut self) -> Option<Self::Item> {
        match self.next_triple() {
            Ok(Some(triple)) => Some(Ok(triple)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        }
    }
}

impl Parser<'_> {
    fn parse_prefix(&mut self) -> Result<(), RdfError> {
        let name_tok = self.consume_tok()?;
        if name_tok.kind != TokenKind::PrefixedName && name_tok.kind != TokenKind::Keyword {
            return Err(RdfError::UnexpectedToken {
                line: name_tok.line,
                col: name_tok.col,
                expected: "prefix name".into(),
                got: format!("{:?}", name_tok.kind),
            });
        }
        let iri_tok = self.expect_tok(TokenKind::Iri)?;
        self.expect_tok(TokenKind::Dot)?;

        let raw = name_tok.value;
        let colon_pos = raw.rfind(':').ok_or(RdfError::InvalidPrefix)?;
        let label = &raw[..colon_pos];
        let iri_inner = Self::extract_iri(iri_tok.value);

        self.prefix_map.insert(label.to_string(), iri_inner);
        Ok(())
    }

    fn parse_base(&mut self) -> Result<(), RdfError> {
        let iri_tok = self.expect_tok(TokenKind::Iri)?;
        self.expect_tok(TokenKind::Dot)?;
        self.base = Some(Self::extract_iri(iri_tok.value));
        Ok(())
    }

    fn parse_statement(&mut self) -> Result<(), RdfError> {
        let subj = self.parse_term()?.ok_or(RdfError::UnexpectedEOF)?;
        self.collect_predicate_object_list(&subj)?;
        let pk = self.peek_tok()?;
        if pk.kind == TokenKind::Dot {
            self.consume_tok()?;
        }
        Ok(())
    }

    fn collect_predicate_object_list(&mut self, subj: &Term) -> Result<(), RdfError> {
        loop {
            let verb = self.parse_verb()?;
            let Some(verb) = verb else { break };

            loop {
                let insert_pos = self.queue.len();
                let obj = self.parse_term()?.ok_or(RdfError::UnexpectedEOF)?;
                let sc = Self::clone_term(subj);
                let pc = Self::clone_term(&verb);
                self.queue.insert(
                    insert_pos,
                    Triple {
                        subject: sc,
                        predicate: pc,
                        object: obj,
                    },
                );

                let pk = self.peek_tok()?;
                if pk.kind == TokenKind::Comma {
                    self.consume_tok()?;
                } else {
                    break;
                }
            }

            let pk = self.peek_tok()?;
            if pk.kind == TokenKind::Semicolon {
                self.consume_tok()?;
                let pk2 = self.peek_tok()?;
                if matches!(
                    pk2.kind,
                    TokenKind::Dot | TokenKind::Eof | TokenKind::BlankNodeClose
                ) {
                    break;
                }
            } else {
                break;
            }
        }
        Ok(())
    }

    fn parse_verb(&mut self) -> Result<Option<Term>, RdfError> {
        let tok = self.peek_tok()?;
        match tok.kind {
            TokenKind::Dot | TokenKind::Eof | TokenKind::BlankNodeClose => return Ok(None),
            _ => {}
        }
        if tok.kind == TokenKind::Keyword && tok.value == "a" {
            self.consume_tok()?;
            return Ok(Some(Term::Iri(RDF_TYPE.to_string())));
        }
        self.parse_term()
    }

    fn parse_term(&mut self) -> Result<Option<Term>, RdfError> {
        let tok = self.peek_tok()?;
        match tok.kind {
            TokenKind::Iri => {
                self.consume_tok()?;
                Ok(Some(Term::Iri(Self::extract_iri(tok.value))))
            }
            TokenKind::PrefixedName => {
                self.consume_tok()?;
                let iri = self.expand_prefixed_name(tok.value);
                Ok(Some(Term::Iri(iri)))
            }
            TokenKind::BlankNode => {
                self.consume_tok()?;
                Ok(Some(Term::BlankNode(tok.value[2..].to_string())))
            }
            TokenKind::BlankNodeOpen => self.parse_inline_blank_node(),
            TokenKind::Literal => {
                self.consume_tok()?;
                self.parse_literal_term(tok)
            }
            TokenKind::Keyword => {
                let kw = tok.value;
                if kw == "true" || kw == "false" {
                    self.consume_tok()?;
                    Ok(Some(Term::Literal(Literal {
                        value: kw.to_string(),
                        lang: None,
                        datatype: Some(format!("{}boolean", crate::XSD_NS)),
                    })))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn parse_literal_term(&mut self, lit_tok: Token) -> Result<Option<Term>, RdfError> {
        let raw = lit_tok.value;

        if !raw.starts_with('"') {
            let value = raw.to_string();
            let has_dot = raw.contains('.');
            let has_exp = raw.contains('e') || raw.contains('E');
            let dt = if has_dot || has_exp {
                format!("{}double", crate::XSD_NS)
            } else {
                format!("{}integer", crate::XSD_NS)
            };
            return Ok(Some(Term::Literal(Literal {
                value,
                lang: None,
                datatype: Some(dt),
            })));
        }

        let content = Self::extract_literal_content(raw);
        let pk = self.peek_tok()?;
        if pk.kind == TokenKind::LangTag {
            self.consume_tok()?;
            return Ok(Some(Term::Literal(Literal {
                value: content,
                lang: Some(pk.value[1..].to_string()),
                datatype: None,
            })));
        }
        if pk.kind == TokenKind::DatatypeMarker {
            self.consume_tok()?;
            let dt_tok = self.consume_tok()?;
            let dt_iri = match dt_tok.kind {
                TokenKind::Iri => Self::extract_iri(dt_tok.value),
                TokenKind::PrefixedName => self.expand_prefixed_name(dt_tok.value),
                _ => {
                    return Err(RdfError::UnexpectedToken {
                        line: dt_tok.line,
                        col: dt_tok.col,
                        expected: "datatype IRI".into(),
                        got: format!("{:?}", dt_tok.kind),
                    });
                }
            };
            return Ok(Some(Term::Literal(Literal {
                value: content,
                lang: None,
                datatype: Some(dt_iri),
            })));
        }
        Ok(Some(Term::Literal(Literal {
            value: content,
            lang: None,
            datatype: None,
        })))
    }

    fn parse_inline_blank_node(&mut self) -> Result<Option<Term>, RdfError> {
        self.expect_tok(TokenKind::BlankNodeOpen)?;
        self.blank_counter += 1;
        let id = format!("b{}", self.blank_counter);
        let bn_term = Term::BlankNode(id);

        let pk = self.peek_tok()?;
        if pk.kind != TokenKind::BlankNodeClose {
            let bn_copy = Self::clone_term(&bn_term);
            self.collect_predicate_object_list(&bn_copy)?;
        }

        self.expect_tok(TokenKind::BlankNodeClose)?;
        Ok(Some(bn_term))
    }

    fn extract_iri(raw: &str) -> String {
        if raw.len() >= 2 && raw.starts_with('<') && raw.ends_with('>') {
            raw[1..raw.len() - 1].to_string()
        } else {
            raw.to_string()
        }
    }

    fn expand_prefixed_name(&self, raw: &str) -> String {
        if let Some(colon) = raw.find(':') {
            let prefix_label = &raw[..colon];
            let local = &raw[colon + 1..];
            if let Some(base_iri) = self.prefix_map.get(prefix_label) {
                return format!("{base_iri}{local}");
            }
            raw.to_string()
        } else {
            if let Some(base_iri) = self.prefix_map.get("") {
                return format!("{base_iri}{raw}");
            }
            raw.to_string()
        }
    }

    fn clone_term(t: &Term) -> Term {
        match t {
            Term::Iri(s) => Term::Iri(s.clone()),
            Term::BlankNode(s) => Term::BlankNode(s.clone()),
            Term::Literal(l) => Term::Literal(Literal {
                value: l.value.clone(),
                lang: l.lang.clone(),
                datatype: l.datatype.clone(),
            }),
        }
    }

    fn extract_literal_content(raw: &str) -> String {
        if raw.len() >= 6 && raw.starts_with("\"\"\"") && raw.ends_with("\"\"\"") {
            raw[3..raw.len() - 3].to_string()
        } else if raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"') {
            raw[1..raw.len() - 1].to_string()
        } else {
            raw.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_triples(src: &str) -> Vec<Triple> {
        let p = Parser::new(src);
        p.collect::<Result<Vec<_>, _>>().unwrap()
    }

    #[test]
    fn test_parse_simple_triple() {
        let trips = parse_triples("<http://s> <http://p> <http://o> .");
        assert_eq!(trips.len(), 1);
        assert_eq!(trips[0].subject, Term::Iri("http://s".into()));
        assert_eq!(trips[0].predicate, Term::Iri("http://p".into()));
        assert_eq!(trips[0].object, Term::Iri("http://o".into()));
    }

    #[test]
    fn test_parse_predicate_object_list() {
        let trips = parse_triples("<http://s> <http://p1> <http://o1> ; <http://p2> <http://o2> .");
        assert_eq!(trips.len(), 2);
        assert_eq!(trips[0].predicate, Term::Iri("http://p1".into()));
        assert_eq!(trips[1].predicate, Term::Iri("http://p2".into()));
    }

    #[test]
    fn test_parse_object_list() {
        let trips = parse_triples("<http://s> <http://p> <http://o1> , <http://o2> .");
        assert_eq!(trips.len(), 2);
        assert_eq!(trips[0].object, Term::Iri("http://o1".into()));
        assert_eq!(trips[1].object, Term::Iri("http://o2".into()));
    }

    #[test]
    fn test_parse_a_shorthand() {
        let trips = parse_triples("<http://s> a <http://Class> .");
        assert_eq!(trips[0].predicate, Term::Iri(RDF_TYPE.into()));
    }

    #[test]
    fn test_parse_prefix_expansion() {
        let trips = parse_triples("@prefix ex: <http://example.org/> .\nex:foo a ex:Thing .");
        assert_eq!(trips[0].subject, Term::Iri("http://example.org/foo".into()));
        assert_eq!(
            trips[0].object,
            Term::Iri("http://example.org/Thing".into())
        );
    }

    #[test]
    fn test_parse_blank_node_subject() {
        let trips = parse_triples("_:b1 <http://p> <http://o> .");
        assert_eq!(trips[0].subject, Term::BlankNode("b1".into()));
    }

    #[test]
    fn test_parse_literal_object() {
        let trips = parse_triples("<http://s> <http://p> \"hello\" .");
        match &trips[0].object {
            Term::Literal(lit) => assert_eq!(lit.value, "hello"),
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_inline_blank_node() {
        let trips = parse_triples("<http://s> <http://p> [ <http://p2> <http://o2> ] .");
        assert!(matches!(trips[0].object, Term::BlankNode(_)));
        assert_eq!(trips[1].predicate, Term::Iri("http://p2".into()));
    }

    #[test]
    fn test_parse_multiple_subjects() {
        let trips =
            parse_triples("<http://a> <http://p> <http://x> .\n<http://b> <http://p> <http://y> .");
        assert_eq!(trips.len(), 2);
        assert_eq!(trips[0].subject, Term::Iri("http://a".into()));
        assert_eq!(trips[1].subject, Term::Iri("http://b".into()));
    }

    #[test]
    fn test_parse_literal_with_lang_tag() {
        let trips = parse_triples("<http://s> <http://p> \"bonjour\"@fr .");
        match &trips[0].object {
            Term::Literal(lit) => {
                assert_eq!(lit.lang, Some("fr".into()));
                assert_eq!(lit.value, "bonjour");
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_literal_with_datatype() {
        let trips = parse_triples(
            "<http://s> <http://p> \"42\"^^<http://www.w3.org/2001/XMLSchema#integer> .",
        );
        match &trips[0].object {
            Term::Literal(lit) => {
                assert_eq!(
                    lit.datatype,
                    Some("http://www.w3.org/2001/XMLSchema#integer".into())
                );
                assert_eq!(lit.value, "42");
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_numeric_literal() {
        let trips = parse_triples("<http://s> <http://p> 42 .");
        match &trips[0].object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "42");
                assert_eq!(lit.datatype, Some(format!("{}integer", crate::XSD_NS)));
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_triple_quoted_literal() {
        let trips = parse_triples("<http://s> <http://p> \"\"\"hello\nworld\"\"\" .");
        match &trips[0].object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "hello\nworld");
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_true_false_literals() {
        let trips = parse_triples("<http://s> <http://p> true .");
        match &trips[0].object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "true");
                assert_eq!(lit.datatype, Some(format!("{}boolean", crate::XSD_NS)));
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_prefixed_name_object() {
        let trips = parse_triples("@prefix ex: <http://example.org/> .\nex:s ex:p ex:o .");
        assert_eq!(trips[0].subject, Term::Iri("http://example.org/s".into()));
    }

    #[test]
    fn test_parse_empty_prefix_map() {
        let trips = parse_triples(":foo <http://p> <http://o> .");
        assert_eq!(trips[0].subject, Term::Iri(":foo".into()));
    }

    #[test]
    fn test_parse_empty_blank_node_brackets() {
        let trips = parse_triples("<http://s> <http://p> [ ] .");
        assert!(matches!(trips[0].object, Term::BlankNode(_)));
        assert_eq!(trips.len(), 1);
    }
}
