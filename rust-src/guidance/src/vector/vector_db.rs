use std::path::Path;
use std::sync::Mutex;

use guidance_common::error::DbError;
use rusqlite::params;
use thiserror::Error;

use super::math;

#[derive(Error, Debug)]
pub enum VectorDbError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("embedding dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
}

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub id: i64,
    pub name: String,
    pub source: String,
    pub signature: Option<String>,
    pub similarity: f32,
}

pub struct GuidanceDb {
    conn: Mutex<rusqlite::Connection>,
}

impl GuidanceDb {
    pub fn open(path: &Path) -> Result<Self, VectorDbError> {
        let conn = rusqlite::Connection::open(path)?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self, VectorDbError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.init_schema()?;
        Ok(db)
    }

    fn init_schema(&self) -> Result<(), VectorDbError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS guidance_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                source TEXT NOT NULL,
                signature TEXT,
                comment TEXT,
                module TEXT NOT NULL,
                language TEXT NOT NULL DEFAULT 'zig',
                embedding BLOB,
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS embedding_cache (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                query_hash TEXT NOT NULL UNIQUE,
                query_text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE INDEX IF NOT EXISTS idx_nodes_name ON guidance_nodes(name);
            CREATE INDEX IF NOT EXISTS idx_nodes_source ON guidance_nodes(source);
            CREATE INDEX IF NOT EXISTS idx_cache_query_hash ON embedding_cache(query_hash);
            ",
        )?;
        Ok(())
    }

    pub fn insert_node(
        &self,
        name: &str,
        source: &str,
        signature: Option<&str>,
        comment: Option<&str>,
        module: &str,
        language: &str,
        embedding: Option<&[f32]>,
    ) -> Result<i64, VectorDbError> {
        let conn = self.conn.lock().unwrap();
        let embedding_blob = embedding.map(math::vec_to_bytes);

        conn.execute(
            "INSERT INTO guidance_nodes (name, source, signature, comment, module, language, embedding)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                name,
                source,
                signature,
                comment,
                module,
                language,
                embedding_blob,
            ],
        )?;

        Ok(conn.last_insert_rowid())
    }

    pub fn vector_search(
        &self,
        query_vec: &[f32],
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, name, source, signature, embedding FROM guidance_nodes WHERE embedding IS NOT NULL",
        )?;

        let mut results: Vec<SearchResult> = Vec::new();

        let rows = stmt.query_map([], |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let source: String = row.get(2)?;
            let signature: Option<String> = row.get(3)?;
            let embedding_blob: Option<Vec<u8>> = row.get(4)?;
            Ok((id, name, source, signature, embedding_blob))
        })?;

        for row_result in rows {
            let (id, name, source, signature, embedding_blob) = row_result?;
            if let Some(blob) = embedding_blob {
                let embedding = math::bytes_to_vec(&blob);
                if embedding.len() != query_vec.len() {
                    continue;
                }
                let similarity = math::cosine_similarity(query_vec, &embedding);
                results.push(SearchResult {
                    id,
                    name,
                    source,
                    signature,
                    similarity,
                });
            }
        }

        results.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        Ok(results)
    }

    pub fn keyword_search(&self, query: &str) -> Result<Vec<SearchResult>, VectorDbError> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query);

        let mut stmt = conn.prepare(
            "SELECT id, name, source, signature FROM guidance_nodes
             WHERE name LIKE ?1 OR signature LIKE ?1 OR comment LIKE ?1
             LIMIT 50",
        )?;

        let results = stmt
            .query_map(params![pattern], |row| {
                Ok(SearchResult {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    source: row.get(2)?,
                    signature: row.get(3)?,
                    similarity: 1.0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    pub fn hybrid_search(
        &self,
        query: &str,
        query_vec: Option<&[f32]>,
        k: usize,
    ) -> Result<Vec<SearchResult>, VectorDbError> {
        let mut keyword_results = self.keyword_search(query)?;

        let mut vector_results = if let Some(vec) = query_vec {
            self.vector_search(vec, k)?
        } else {
            Vec::new()
        };

        let mut seen = std::collections::HashSet::new();
        let mut fused: Vec<SearchResult> = Vec::new();

        let kw_weight = 0.35;
        let vec_weight = 0.65;

        for result in keyword_results.iter_mut() {
            result.similarity *= kw_weight;
        }

        for result in vector_results.iter_mut() {
            result.similarity *= vec_weight;
        }

        let all_results: Vec<Vec<SearchResult>> = vec![keyword_results, vector_results];

        for mut batch in all_results {
            for result in batch.drain(..) {
                if seen.insert(result.id) {
                    fused.push(result);
                }
            }
        }

        fused.sort_by(|a, b| b.similarity.partial_cmp(&a.similarity).unwrap_or(std::cmp::Ordering::Equal));
        fused.truncate(k);

        Ok(fused)
    }

    pub fn get_node_count(&self) -> Result<i64, VectorDbError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM guidance_nodes", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn get_embedding_count(&self) -> Result<i64, VectorDbError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM guidance_nodes WHERE embedding IS NOT NULL", [], |row| {
                row.get(0)
            })?;
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db() -> GuidanceDb {
        GuidanceDb::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn test_insert_and_count() {
        let db = make_db();
        let id = db
            .insert_node("hello", "src/test.zig", Some("fn hello() void"), Some("Says hello"), "test", "zig", None)
            .expect("insert");
        assert!(id > 0);
        assert_eq!(db.get_node_count().expect("count"), 1);
    }

    #[test]
    fn test_keyword_search() {
        let db = make_db();
        db.insert_node("greet", "src/test.zig", Some("fn greet() void"), Some("Greets the user"), "test", "zig", None)
            .expect("insert");
        db.insert_node("add", "src/math.zig", Some("fn add() i32"), Some("Adds numbers"), "math", "zig", None)
            .expect("insert");

        let results = db.keyword_search("greet").expect("search");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "greet");
    }

    #[test]
    fn test_vector_search() {
        let db = make_db();
        let emb1: Vec<f32> = (0..4).map(|i| i as f32).collect();
        let emb2: Vec<f32> = (0..4).map(|i| (i + 10) as f32).collect();

        db.insert_node("a", "src/a.zig", None, None, "test", "zig", Some(&emb1))
            .expect("insert");
        db.insert_node("b", "src/b.zig", None, None, "test", "zig", Some(&emb2))
            .expect("insert");

        let query = vec![0.5, 1.5, 2.5, 3.5];
        let results = db.vector_search(&query, 2).expect("search");
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].name, "a");
    }

    #[test]
    fn test_empty_search() {
        let db = make_db();
        let results = db.keyword_search("nonexistent").expect("search");
        assert!(results.is_empty());
    }

    #[test]
    fn test_hybrid_search() {
        let db = make_db();
        let emb = vec![0.1, 0.2, 0.3, 0.4];

        db.insert_node("hello_fn", "src/test.zig", Some("fn hello() void"), Some("Says hello"), "test", "zig", Some(&emb))
            .expect("insert");

        let results = db
            .hybrid_search("hello", Some(&emb), 5)
            .expect("hybrid search");
        assert!(!results.is_empty());
    }
}
