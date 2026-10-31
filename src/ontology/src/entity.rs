use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityType {
    Person,
    Project,
    Location,
    Uncertain,
}

#[derive(Debug, Clone)]
pub struct EntityFreq {
    pub name: String,
    pub frequency: usize,
    pub entity_type: EntityType,
}

use std::sync::LazyLock;

/// Lexicon stoplist loaded from `data/entity_stoplist.txt` (one word per
/// line). The file is the single home; the `LazyLock` set is the compiled
/// view. Confidence heuristic: membership means "not a name", never a
/// task-value judgment.
static ENTITY_STOPLIST: LazyLock<std::collections::HashSet<&'static str>> = LazyLock::new(|| {
    const WORDS: &str = include_str!("../data/entity_stoplist.txt");
    let mut s = std::collections::HashSet::new();
    for line in WORDS.lines() {
        let w = line.trim();
        if !w.is_empty() {
            s.insert(Box::leak(w.to_string().into_boxed_str()) as &'static str);
        }
    }
    s
});

fn is_capitalized(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_uppercase)
}

fn is_entity_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '-' || c == '\'' || c == '.'
}

pub fn candidate_entity_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if is_entity_word_char(c) {
            current.push(c);
        } else {
            if current.len() >= 2
                && is_capitalized(&current)
                && !ENTITY_STOPLIST.contains(current.as_str())
            {
                words.push(current.clone());
            }
            current.clear();
        }
    }
    if current.len() >= 2 && is_capitalized(&current) && !ENTITY_STOPLIST.contains(current.as_str())
    {
        words.push(current);
    }
    words
}

pub fn extract_entities(content: &str, min_frequency: usize) -> Vec<EntityFreq> {
    let mut freq: HashMap<String, usize> = HashMap::new();
    for word in candidate_entity_words(content) {
        *freq.entry(word).or_insert(0) += 1;
    }
    let mut result: Vec<EntityFreq> = freq
        .into_iter()
        .filter(|(_, count)| *count >= min_frequency)
        .map(|(name, frequency)| EntityFreq {
            name,
            frequency,
            entity_type: EntityType::Uncertain,
        })
        .collect();
    result.sort_by_key(|b| std::cmp::Reverse(b.frequency));
    result
}

#[cfg(test)]
#[path = "../tests/entity.rs"]
mod tests;

#[cfg(test)]
#[path = "../tests/entity_calibration.rs"]
mod entity_calibration_tests;
