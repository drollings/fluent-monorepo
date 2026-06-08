use guidance_common::types::GuidanceDoc;

use super::identifier;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum QueryIntent {
    IdentifierLookup,
    CapabilityQuery,
    ConceptQuery,
    GeneralSearch,
}

#[derive(Debug, Clone)]
pub struct QueryMatch {
    pub intent: QueryIntent,
    pub priority: u8,
    pub matched: bool,
}

pub fn classify_query(query: &str) -> QueryIntent {
    let trimmed = query.trim();

    if trimmed.is_empty() {
        return QueryIntent::GeneralSearch;
    }

    if identifier::detect_identifier_pattern(trimmed).is_some() {
        return QueryIntent::IdentifierLookup;
    }

    let word_count = trimmed.split_whitespace().count();

    if (2..=4).contains(&word_count) {
        QueryIntent::CapabilityQuery
    } else if word_count >= 2 {
        QueryIntent::ConceptQuery
    } else {
        QueryIntent::GeneralSearch
    }
}

pub fn matches(query: &str, _db: &GuidanceDoc) -> QueryMatch {
    let intent = classify_query(query);
    let priority: u8 = match intent {
        QueryIntent::IdentifierLookup => 0,
        QueryIntent::CapabilityQuery => 2,
        QueryIntent::ConceptQuery => 4,
        QueryIntent::GeneralSearch => 6,
    };

    QueryMatch {
        intent,
        priority,
        matched: intent != QueryIntent::GeneralSearch,
    }
}

pub fn query_strategy_priority(intent: QueryIntent) -> u8 {
    match intent {
        QueryIntent::IdentifierLookup => 0,
        QueryIntent::CapabilityQuery => 2,
        QueryIntent::ConceptQuery => 4,
        QueryIntent::GeneralSearch => 6,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_identifier() {
        assert_eq!(classify_query("helloWorld"), QueryIntent::IdentifierLookup);
        assert_eq!(classify_query("snake_case_fn"), QueryIntent::IdentifierLookup);
        assert_eq!(classify_query("PascalCaseType"), QueryIntent::IdentifierLookup);
    }

    #[test]
    fn test_classify_capability() {
        assert_eq!(classify_query("target registry"), QueryIntent::CapabilityQuery);
        assert_eq!(classify_query("ast parsing zig"), QueryIntent::CapabilityQuery);
    }

    #[test]
    fn test_classify_concept() {
        assert_eq!(classify_query("how do I parse a zig file"), QueryIntent::ConceptQuery);
        assert_eq!(classify_query("what is the target registry used for"), QueryIntent::ConceptQuery);
    }

    #[test]
    fn test_classify_empty() {
        assert_eq!(classify_query(""), QueryIntent::GeneralSearch);
        assert_eq!(classify_query("   "), QueryIntent::GeneralSearch);
    }

    #[test]
    fn test_matches() {
        let doc = GuidanceDoc::default();
        let m = matches("helloWorld", &doc);
        assert!(m.matched);
        assert_eq!(m.intent, QueryIntent::IdentifierLookup);
    }

    #[test]
    fn test_priority_order() {
        assert!(
            query_strategy_priority(QueryIntent::IdentifierLookup)
                < query_strategy_priority(QueryIntent::CapabilityQuery)
        );
        assert!(
            query_strategy_priority(QueryIntent::CapabilityQuery)
                < query_strategy_priority(QueryIntent::ConceptQuery)
        );
    }
}
