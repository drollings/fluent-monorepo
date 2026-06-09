use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SemanticAliases {
    alias_map: HashMap<String, Vec<String>>,
}

impl SemanticAliases {
    pub fn new() -> Self { Self { alias_map: HashMap::new() } }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        Ok(Self { alias_map: serde_json::from_str(json)? })
    }

    pub fn expand(&self, token: &str) -> Vec<String> {
        let mut results = vec![token.to_string()];
        if let Some(aliases) = self.alias_map.get(token) {
            results.extend(aliases.iter().cloned());
        }
        results
    }

    pub fn expand_query(&self, query: &str) -> Vec<String> {
        let tokens: Vec<&str> = query.split_whitespace().collect();
        if tokens.len() == 1 { return self.expand(tokens[0]); }
        let expansions: Vec<Vec<String>> = tokens.iter().map(|t| self.expand(t)).collect();
        let mut results = Vec::new();
        Self::cartesian_product(&expansions, 0, &mut String::new(), &mut results);
        results
    }

    fn cartesian_product(lists: &[Vec<String>], depth: usize, current: &mut String, results: &mut Vec<String>) {
        if depth == lists.len() { if !current.is_empty() { results.push(current.clone()); } return; }
        for item in &lists[depth] {
            let start = current.len();
            if depth > 0 { current.push(' '); }
            current.push_str(item);
            Self::cartesian_product(lists, depth + 1, current, results);
            current.truncate(start);
        }
    }

    pub fn insert(&mut self, key: &str, aliases: Vec<String>) { self.alias_map.insert(key.to_string(), aliases); }
    pub fn get(&self, key: &str) -> Option<&Vec<String>> { self.alias_map.get(key) }
}

impl Default for SemanticAliases { fn default() -> Self { Self::new() } }

#[cfg(test)]
mod tests {
    use super::*;

    #[test] fn test_empty_aliases() { assert_eq!(SemanticAliases::new().expand("hello"), vec!["hello"]); }
    #[test] fn test_single_alias() {
        let mut a = SemanticAliases::new();
        a.insert("hello", vec!["hi".into(), "greetings".into()]);
        let expanded = a.expand("hello");
        assert!(expanded.contains(&"hello".to_string()));
        assert!(expanded.contains(&"hi".to_string()));
    }
    #[test] fn test_from_json() {
        let a = SemanticAliases::from_json(r#"{"hello": ["hi", "hey"]}"#).unwrap();
        assert_eq!(a.get("hello").unwrap().len(), 2);
    }
    #[test] fn test_expand_query_multi() {
        let mut a = SemanticAliases::new();
        a.insert("fn", vec!["function".into()]);
        a.insert("arg", vec!["argument".into()]);
        assert!(a.expand_query("fn arg").len() >= 2);
    }
}
