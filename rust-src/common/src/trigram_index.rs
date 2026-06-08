use std::collections::HashMap;

pub type Trigram = u32;

#[derive(Debug, Clone)]
pub struct TrigramHit {
    pub doc_id: u32,
    pub position: u32,
}

pub const MAX_POSTINGS: u16 = 512;
pub const TRIGRAM_INDEX_MAGIC: u32 = 0x54524947;
pub const TRIGRAM_INDEX_VERSION: u32 = 1;

fn make_trigram(a: u8, b: u8, c: u8) -> Trigram {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16)
}

#[derive(Debug, Clone)]
pub struct TrigramIndex {
    index: HashMap<Trigram, Vec<TrigramHit>>,
    doc_count: u32,
}

impl TrigramIndex {
    pub fn new() -> Self {
        Self {
            index: HashMap::new(),
            doc_count: 0,
        }
    }

    pub fn build_from_content(&mut self, path: &str, content: &str) {
        let bytes = content.as_bytes();
        if bytes.len() < 3 {
            return;
        }
        let doc_id = self.doc_count;
        self.doc_count += 1;
        for i in 0..=bytes.len() - 3 {
            let tri = make_trigram(bytes[i], bytes[i + 1], bytes[i + 2]);
            let hit = TrigramHit {
                doc_id,
                position: i as u32,
            };
            self.index.entry(tri).or_default().push(hit);
        }
        let _ = path;
    }

    pub fn search_bytes(&self, tri_bytes: [u8; 3]) -> &[TrigramHit] {
        let tri = make_trigram(tri_bytes[0], tri_bytes[1], tri_bytes[2]);
        self.index.get(&tri).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn search_trigram(&self, tri: Trigram) -> &[TrigramHit] {
        self.index.get(&tri).map(|v| v.as_slice()).unwrap_or(&[])
    }

    pub fn candidates(&self, query: &str) -> Vec<u32> {
        let bytes = query.as_bytes();
        if bytes.len() < 3 {
            return Vec::new();
        }
        let first = make_trigram(bytes[0], bytes[1], bytes[2]);
        let hits = self.index.get(&first).map(|v| v.as_slice()).unwrap_or(&[]);
        let mut doc_ids: Vec<u32> = hits.iter().map(|h| h.doc_id).collect();
        doc_ids.sort_unstable();
        doc_ids.dedup();
        doc_ids
    }

    pub fn search(&self, query: &str) -> Vec<u32> {
        self.candidates(query)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn make_trigram_value() {
        let tri = make_trigram(b'a', b'b', b'c');
        assert_eq!(tri, 0x636261);
    }

    #[test]
    fn build_and_search() {
        let mut idx = TrigramIndex::new();
        idx.build_from_content("hello.txt", "hello world");
        let hits = idx.search_bytes([b'h', b'e', b'l']);
        assert!(!hits.is_empty());
    }

    #[test]
    fn candidates() {
        let mut idx = TrigramIndex::new();
        idx.build_from_content("a.txt", "hello world");
        let docs = idx.candidates("hello");
        assert_eq!(docs.len(), 1);
    }
}
