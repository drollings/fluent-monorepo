use crate::lexer::{Lexer, TokenKind};
use crate::parser::{Literal, Term};
use crate::RdfError;

#[derive(Debug, Clone, PartialEq)]
pub struct Quad {
    pub subject: Term,
    pub predicate: Term,
    pub object: Term,
    pub graph: Option<Term>,
}

pub struct NQuadsParser;

impl NQuadsParser {
    pub fn parse_line(line: &str) -> Result<Option<Quad>, RdfError> {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return Ok(None);
        }

        let mut lex = Lexer::new(line);
        let mut terms: Vec<Term> = Vec::new();
        let mut graph: Option<Term> = None;

        loop {
            let tok = lex.next_token()?;
            match tok.kind {
                TokenKind::Eof => break,
                TokenKind::Dot => break,
                TokenKind::Iri => {
                    let iri = Self::extract_iri(tok.value);
                    terms.push(Term::Iri(iri));
                }
                TokenKind::PrefixedName => {
                    let iri = tok.value.to_string();
                    terms.push(Term::Iri(iri));
                }
                TokenKind::BlankNode => {
                    let id = tok.value[2..].to_string();
                    terms.push(Term::BlankNode(id));
                }
                TokenKind::Literal => {
                    let lit = Self::parse_literal(&mut lex, tok.value)?;
                    terms.push(Term::Literal(lit));
                }
                _ => {
                    return Err(RdfError::UnexpectedToken {
                        line: tok.line,
                        col: tok.col,
                        expected: "term".into(),
                        got: format!("{:?}", tok.kind),
                    });
                }
            }
        }

        if terms.len() < 3 {
            return Ok(None);
        }

        if terms.len() > 3 {
            graph = Some(terms.remove(3));
        }

        if terms.len() >= 3 {
            Ok(Some(Quad {
                subject: terms.remove(0),
                predicate: terms.remove(0),
                object: terms.remove(0),
                graph,
            }))
        } else {
            Ok(None)
        }
    }

    fn extract_iri(raw: &str) -> String {
        if raw.len() >= 2 && raw.starts_with('<') && raw.ends_with('>') {
            raw[1..raw.len() - 1].to_string()
        } else {
            raw.to_string()
        }
    }

    fn parse_literal(lex: &mut Lexer, value: &str) -> Result<Literal, RdfError> {
        let content = Self::extract_literal_content(value);
        let pk = lex.next_token()?;
        if pk.kind == TokenKind::LangTag {
            let lang = pk.value[1..].to_string();
            return Ok(Literal { value: content, lang: Some(lang), datatype: None });
        }
        if pk.kind == TokenKind::DatatypeMarker {
            let dt_tok = lex.next_token()?;
            let dt_iri = match dt_tok.kind {
                TokenKind::Iri => Self::extract_iri(dt_tok.value),
                TokenKind::PrefixedName => dt_tok.value.to_string(),
                _ => {
                    return Err(RdfError::UnexpectedToken {
                        line: dt_tok.line,
                        col: dt_tok.col,
                        expected: "datatype IRI".into(),
                        got: format!("{:?}", dt_tok.kind),
                    });
                }
            };
            return Ok(Literal { value: content, lang: None, datatype: Some(dt_iri) });
        }
        Ok(Literal { value: content, lang: None, datatype: None })
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

    #[test]
    fn test_parse_line_simple() {
        let line = "<http://s> <http://p> <http://o> .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        assert_eq!(quad.subject, Term::Iri("http://s".into()));
        assert_eq!(quad.predicate, Term::Iri("http://p".into()));
        assert_eq!(quad.object, Term::Iri("http://o".into()));
        assert!(quad.graph.is_none());
    }

    #[test]
    fn test_parse_line_with_graph() {
        let line = "<http://s> <http://p> <http://o> <http://g> .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        assert_eq!(quad.graph, Some(Term::Iri("http://g".into())));
    }

    #[test]
    fn test_parse_line_blank_node() {
        let line = "_:s <http://p> _:o .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        assert_eq!(quad.subject, Term::BlankNode("s".into()));
        assert_eq!(quad.object, Term::BlankNode("o".into()));
    }

    #[test]
    fn test_parse_line_literal() {
        let line = "<http://s> <http://p> \"hello\" .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        match quad.object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "hello");
                assert!(lit.lang.is_none());
                assert!(lit.datatype.is_none());
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_line_skip_comment() {
        let result = NQuadsParser::parse_line("# this is a comment").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_line_skip_empty() {
        let result = NQuadsParser::parse_line("").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_line_literal_with_lang() {
        let line = "<http://s> <http://p> \"bonjour\"@fr .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        match quad.object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "bonjour");
                assert_eq!(lit.lang, Some("fr".into()));
            }
            _ => panic!("expected literal"),
        }
    }

    #[test]
    fn test_parse_line_literal_with_datatype() {
        let line = "<http://s> <http://p> \"42\"^^<http://www.w3.org/2001/XMLSchema#integer> .";
        let quad = NQuadsParser::parse_line(line).unwrap().unwrap();
        match quad.object {
            Term::Literal(lit) => {
                assert_eq!(lit.value, "42");
                assert_eq!(lit.datatype, Some("http://www.w3.org/2001/XMLSchema#integer".into()));
            }
            _ => panic!("expected literal"),
        }
    }
}
