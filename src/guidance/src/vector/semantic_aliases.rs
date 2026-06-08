use std::collections::HashMap;

#[derive(Debug, Clone)]
pub struct SemanticAliases {
    alias_map: HashMap<String, Vec<String>>,
}

impl SemanticAliases {
    pub fn new() -> Self {
        Self {
            alias_map: HashMap::new(),
        }
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let alias_map: HashMap<String, Vec<String>> = serde_json::from_str(json)?;
        Ok(Self { alias_map })
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

        if tokens.len() == 1 {
            return self.expand(tokens[0]);
        }

        let mut expansions: Vec<Vec<String>> = Vec::new();
        for token in &tokens {
            expansions.push(self.expand(token));
        }

        let mut results = Vec::new();
        self.cartesian_product(&expansions, 0, &mut String::new(), &mut results);
        results
    }

    fn cartesian_product(
        &self,
        lists: &[Vec<String>],
        depth: usize,
        current: &mut String,
        results: &mut Vec<String>,
    ) {
        if depth == lists.len() {
            if !current.is_empty() {
                results.push(current.clone());
            }
            return;
        }

        for item in &lists[depth] {
            let start_len = current.len();
            if depth > 0 {
                current.push(' ');
            }
            current.push_str(item);
            self.cartesian_product(lists, depth + 1, current, results);
            current.truncate(start_len);
        }
    }

    pub fn insert(&mut self, key: &str, aliases: Vec<String>) {
        self.alias_map.insert(key.to_string(), aliases);
    }

    pub fn get(&self, key: &str) -> Option<&Vec<String>> {
        self.alias_map.get(key)
    }
}

impl Default for SemanticAliases {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_aliases() {
        let aliases = SemanticAliases::new();
        let expanded = aliases.expand("hello");
        assert_eq!(expanded, vec!["hello"]);
    }

    #[test]
    fn test_single_alias() {
        let mut aliases = SemanticAliases::new();
        aliases.insert("hello", vec!["hi".into(), "greetings".into()]);
        let expanded = aliases.expand("hello");
        assert!(expanded.contains(&"hello".to_string()));
        assert!(expanded.contains(&"hi".to_string()));
        assert!(expanded.contains(&"greetings".to_string()));
    }

    #[test]
    fn test_from_json() {
        let json = r#"{"hello": ["hi", "hey"], "world": ["earth"]}"#;
        let aliases = SemanticAliases::from_json(json).expect("parse");
        assert!(aliases.get("hello").is_some());
        assert_eq!(aliases.get("hello").unwrap().len(), 2);
    }

    #[test]
    fn test_expand_query_single() {
        let mut aliases = SemanticAliases::new();
        aliases.insert("fn", vec!["function".into(), "func".into()]);
        let expanded = aliases.expand_query("fn");
        assert!(expanded.len() >= 2);
    }

    #[test]
    fn test_expand_query_multi() {
        let mut aliases = SemanticAliases::new();
        aliases.insert("fn", vec!["function".into()]);
        aliases.insert("arg", vec!["argument".into()]);
        let expanded = aliases.expand_query("fn arg");
        assert!(expanded.len() >= 2);
    }
}
