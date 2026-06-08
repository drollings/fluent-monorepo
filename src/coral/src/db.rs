use std::mem::size_of;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;

use bitvec::vec::BitVec;
use guidance_common::embeddings::EmbeddingProvider;
use guidance_common::error::DbError;
use guidance_common::types::{ContextNode, GraphNode, KnnHit, NodeId, WasmTool};
use rusqlite::params;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum LibraryError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database error: {0}")]
    Db(#[from] DbError),
    #[error("node not found: {0}")]
    NodeNotFound(String),
    #[error("duplicate node: {0}")]
    DuplicateNode(String),
}

pub struct Library {
    conn: Mutex<rusqlite::Connection>,
}

impl Library {
    pub fn open(path: &Path) -> Result<Self, LibraryError> {
        let conn = rusqlite::Connection::open(path)?;
        let lib = Self {
            conn: Mutex::new(conn),
        };
        lib.init_schema()?;
        Ok(lib)
    }

    pub fn open_in_memory() -> Result<Self, LibraryError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        let lib = Self {
            conn: Mutex::new(conn),
        };
        lib.init_schema()?;
        Ok(lib)
    }

    pub fn init_schema(&self) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS context_nodes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                source TEXT NOT NULL DEFAULT '',
                lod TEXT NOT NULL DEFAULT '[]',
                embedding BLOB,
                capabilities BLOB,
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS edges (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                source_node_id INTEGER NOT NULL,
                target_node_id INTEGER NOT NULL,
                edge_type TEXT NOT NULL DEFAULT 'depends',
                weight REAL NOT NULL DEFAULT 1.0,
                FOREIGN KEY (source_node_id) REFERENCES context_nodes(id),
                FOREIGN KEY (target_node_id) REFERENCES context_nodes(id)
            );

            CREATE TABLE IF NOT EXISTS wasm_tools (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                path TEXT NOT NULL,
                capabilities TEXT NOT NULL DEFAULT '[]'
            );

            CREATE TABLE IF NOT EXISTS targets (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL UNIQUE,
                bit_index INTEGER NOT NULL,
                depends BLOB,
                provides BLOB,
                essential INTEGER NOT NULL DEFAULT 0,
                command TEXT NOT NULL DEFAULT ''
            );

            CREATE TABLE IF NOT EXISTS embedding_cache (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                query_hash TEXT NOT NULL UNIQUE,
                query_text TEXT NOT NULL,
                embedding BLOB NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            );

            CREATE TABLE IF NOT EXISTS entity_types (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                node_id INTEGER NOT NULL,
                type_iri TEXT NOT NULL,
                FOREIGN KEY (node_id) REFERENCES context_nodes(id)
            );

            CREATE TABLE IF NOT EXISTS entity_hierarchy (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                subclass_iri TEXT NOT NULL,
                superclass_iri TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_edges_source ON edges(source_node_id);
            CREATE INDEX IF NOT EXISTS idx_edges_target ON edges(target_node_id);
            CREATE INDEX IF NOT EXISTS idx_nodes_name ON context_nodes(name);
            CREATE INDEX IF NOT EXISTS idx_entity_types_node ON entity_types(node_id);
            CREATE INDEX IF NOT EXISTS idx_entity_types_iri ON entity_types(type_iri);
            ",
        )?;
        Ok(())
    }

    pub fn insert_node(&self, node: &ContextNode) -> Result<NodeId, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let lod_json = serde_json::to_string(&node.lod).unwrap_or_default();
        let embedding_blob = node.embedding.as_ref().map(|v| vec_to_blob(v));
        let capabilities_blob = node.capabilities.as_deref();

        conn.execute(
            "INSERT INTO context_nodes (name, source, lod, embedding, capabilities) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![node.name.as_str(), node.source, lod_json, embedding_blob, capabilities_blob],
        )?;

        Ok(NodeId::from_int(conn.last_insert_rowid()))
    }

    pub fn find_node_by_name(&self, name: &str) -> Result<Option<NodeId>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM context_nodes WHERE name = ?1")?;
        let result = stmt
            .query_row(params![name], |row| {
                let id: i64 = row.get(0)?;
                Ok(NodeId::from_int(id))
            })
            .ok();
        Ok(result)
    }

    pub fn get_node(&self, node_id: NodeId) -> Result<Option<ContextNode>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt =
            conn.prepare("SELECT id, name, source, lod, embedding, capabilities FROM context_nodes WHERE id = ?1")?;
        let result = stmt
            .query_row(params![node_id.as_int()], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let source: String = row.get(2)?;
                let lod_json: String = row.get(3)?;
                let embedding_blob: Option<Vec<u8>> = row.get(4)?;
                let capabilities_blob: Option<Vec<u8>> = row.get(5)?;
                let lod: Vec<String> = serde_json::from_str(&lod_json).unwrap_or_default();
                let embedding = embedding_blob.and_then(|b| blob_to_vec(&b));
                Ok(ContextNode {
                    id: Some(NodeId::from_int(id)),
                    name: name.as_str().into(),
                    source,
                    lod,
                    embedding,
                    capabilities: capabilities_blob,
                })
            })
            .ok();
        Ok(result)
    }

    pub fn knn_search(&self, query_vec: &[f32], k: usize, capability_filter: Option<&BitVec>) -> Result<Vec<KnnHit>, LibraryError> {
        if query_vec.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn.lock().unwrap();
        let capabilities_col = if capability_filter.is_some() { ", capabilities" } else { "" };
        let sql = format!("SELECT id, name, embedding{capabilities_col} FROM context_nodes WHERE embedding IS NOT NULL");
        let mut stmt = conn.prepare(&sql)?;

        let mut results: Vec<KnnHit> = Vec::new();

        let rows = stmt.query_map([], move |row| {
            let id: i64 = row.get(0)?;
            let name: String = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            let caps_blob: Option<Vec<u8>> = if capability_filter.is_some() {
                row.get(3).ok().flatten()
            } else {
                None
            };
            Ok((id, name, blob, caps_blob))
        })?;

        for row_result in rows {
            let (id, name, blob, caps_blob) = row_result?;
            if let Some(filter) = capability_filter {
                let node_bv = caps_blob.as_deref()
                    .map(blob_to_bitvec)
                    .unwrap_or_default();
                let overlap = node_bv.iter()
                    .zip(filter.iter())
                    .any(|(a, b)| *a && *b);
                if !overlap {
                    continue;
                }
            }
            if let Some(emb) = blob_to_vec(&blob) {
                if emb.len() != query_vec.len() {
                    continue;
                }
                let distance = cosine_distance(query_vec, &emb);
                results.push(KnnHit {
                    node_id: NodeId::from_int(id),
                    distance,
                    name: name.as_str().into(),
                });
            }
        }

        results.sort_by(|a, b| a.distance.partial_cmp(&b.distance).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);
        Ok(results)
    }

    pub fn traverse_from(
        &self,
        node_id: NodeId,
        max_depth: u8,
    ) -> Result<Vec<GraphNode>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "WITH RECURSIVE
                traverse(id, name, depth) AS (
                    SELECT n.id, n.name, 0
                    FROM context_nodes n
                    WHERE n.id = ?1
                    UNION ALL
                    SELECT n.id, n.name, t.depth + 1
                    FROM traverse t
                    JOIN edges e ON e.source_node_id = t.id
                    JOIN context_nodes n ON n.id = e.target_node_id
                    WHERE t.depth < ?2
                )
            SELECT DISTINCT id, name, depth FROM traverse ORDER BY depth, name",
        )?;

        let results = stmt
            .query_map(params![node_id.as_int(), max_depth], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                let depth: u32 = row.get(2)?;
                Ok(GraphNode {
                    node_id: NodeId::from_int(id),
                    name: name.as_str().into(),
                    depth,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(results)
    }

    pub fn insert_edge(
        &self,
        source: NodeId,
        target: NodeId,
        edge_type: &str,
        weight: f64,
    ) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO edges (source_node_id, target_node_id, edge_type, weight) VALUES (?1, ?2, ?3, ?4)",
            params![source.as_int(), target.as_int(), edge_type, weight],
        )?;
        Ok(())
    }

    pub fn insert_wasm_tool(&self, tool: &WasmTool) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        let caps_json = serde_json::to_string(&tool.capabilities).unwrap_or_default();
        conn.execute(
            "INSERT INTO wasm_tools (name, path, capabilities) VALUES (?1, ?2, ?3)",
            params![tool.name.as_str(), tool.path, caps_json],
        )?;
        Ok(())
    }

    pub fn find_wasm_tools_by_capability(&self, capability: &str) -> Result<Vec<WasmTool>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT name, path, capabilities FROM wasm_tools")?;

        let results = stmt
            .query_map([], |row| {
                let name: String = row.get(0)?;
                let path: String = row.get(1)?;
                let caps_json: String = row.get(2)?;
                let capabilities: Vec<String> = serde_json::from_str(&caps_json).unwrap_or_default();
                Ok((name, path, capabilities))
            })?
            .filter_map(|r| {
                r.ok().and_then(|(name, path, caps)| {
                    if caps.iter().any(|c| c == capability) {
                        Some(WasmTool {
                            name: name.as_str().into(),
                            path,
                            capabilities: caps.into_iter().map(|c| c.as_str().into()).collect(),
                        })
                    } else {
                        None
                    }
                })
            })
            .collect();

        Ok(results)
    }

    pub fn cache_embedding(
        &self,
        query_hash: &str,
        query_text: &str,
        embedding: &[f32],
    ) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        let blob = vec_to_blob(embedding);
        conn.execute(
            "INSERT OR REPLACE INTO embedding_cache (query_hash, query_text, embedding) VALUES (?1, ?2, ?3)",
            params![query_hash, query_text, blob],
        )?;
        Ok(())
    }

    pub fn get_cached_embedding(&self, query_hash: &str) -> Result<Option<Vec<f32>>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let result = conn
            .query_row(
                "SELECT embedding FROM embedding_cache WHERE query_hash = ?1",
                params![query_hash],
                |row| {
                    let blob: Vec<u8> = row.get(0)?;
                    Ok(blob_to_vec(&blob))
                },
            )
            .ok()
            .flatten();
        Ok(result)
    }

    pub fn node_count(&self) -> Result<i64, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM context_nodes", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn edge_count(&self) -> Result<i64, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM edges", [], |row| row.get(0))?;
        Ok(count)
    }

    pub fn get_all_node_ids(&self) -> Result<Vec<NodeId>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT id FROM context_nodes ORDER BY id")?;
        let ids = stmt
            .query_map([], |row| {
                let id: i64 = row.get(0)?;
                Ok(NodeId::from_int(id))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    pub fn keyword_search(&self, query: &str) -> Result<Vec<KnnHit>, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let pattern = format!("%{}%", query);
        let mut stmt = conn.prepare(
            "SELECT id, name FROM context_nodes WHERE name LIKE ?1 OR source LIKE ?1 LIMIT 10",
        )?;
        let results = stmt
            .query_map(params![pattern], |row| {
                let id: i64 = row.get(0)?;
                let name: String = row.get(1)?;
                Ok(KnnHit {
                    node_id: NodeId::from_int(id),
                    distance: 0.0,
                    name: name.as_str().into(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(results)
    }

    pub fn insert_nodes_batch(&self, nodes: &[ContextNode]) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        let tx = conn.unchecked_transaction()?;
        for node in nodes {
            let lod_json = serde_json::to_string(&node.lod).unwrap_or_default();
            let embedding_blob = node.embedding.as_ref().map(|v| vec_to_blob(v));
            let capabilities_blob = node.capabilities.as_deref();
            tx.execute(
                "INSERT INTO context_nodes (name, source, lod, embedding, capabilities) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![node.name.as_str(), node.source, lod_json, embedding_blob, capabilities_blob],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Insert an entity type mapping (node_id -> type_iri)
    pub fn insert_entity_type(&self, node_id: NodeId, type_iri: &str) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entity_types (node_id, type_iri) VALUES (?1, ?2)",
            params![node_id.as_int(), type_iri],
        )?;
        Ok(())
    }

    /// Insert an entity hierarchy relationship (subclass -> superclass)
    pub fn insert_entity_hierarchy(&self, subclass_iri: &str, superclass_iri: &str) -> Result<(), LibraryError> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO entity_hierarchy (subclass_iri, superclass_iri) VALUES (?1, ?2)",
            params![subclass_iri, superclass_iri],
        )?;
        Ok(())
    }

    /// Duck-typing query: checks if a node has a given type_iri (or any of its supertypes)
    /// via recursive CTE through entity_types + entity_hierarchy tables.
    pub fn is_a(&self, child_id: NodeId, parent_type_iri: &str) -> Result<bool, LibraryError> {
        let conn = self.conn.lock().unwrap();
        let result: bool = conn.query_row(
            "WITH RECURSIVE ancestors(type_iri) AS (
                SELECT type_iri FROM entity_types WHERE node_id = ?1
                UNION
                SELECT eh.superclass_iri FROM ancestors a
                JOIN entity_hierarchy eh ON a.type_iri = eh.subclass_iri
            )
            SELECT COUNT(*) > 0 FROM ancestors WHERE type_iri = ?2",
            params![child_id.as_int(), parent_type_iri],
            |row| row.get(0),
        )?;
        Ok(result)
    }
}

/// HydrationPipeline: on node insert, automatically compute embedding
/// and discover neighbor edges via KNN search.
pub struct HydrationPipeline<'a> {
    library: &'a Library,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl<'a> HydrationPipeline<'a> {
    pub fn new(library: &'a Library, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        Self { library, embedder }
    }

    /// Insert a node, compute its embedding, and auto-discover neighbors.
    /// Returns the new node's ID.
    pub fn insert_and_hydrate(&self, node: &mut ContextNode) -> Result<NodeId, LibraryError> {
        // 1. Compute embedding from LOD[0] (most detailed text)
        let text = node.lod.first().map(|s| s.as_str()).unwrap_or("");
        if !text.is_empty() {
            if let Ok(emb) = self.embedder.embed(text) {
                node.embedding = Some(emb);
            }
        }

        // 2. Insert node
        let node_id = self.library.insert_node(node)?;

        // 3. KNN search for auto-discovered neighbors
        if let Some(ref emb) = node.embedding {
            if let Ok(hits) = self.library.knn_search(emb, 10, None) {
                for hit in hits {
                    if hit.node_id != node_id && hit.distance < 0.3 {
                        let _ = self.library.insert_edge(node_id, hit.node_id, "neighbor_of", hit.distance as f64);
                    }
                }
            }
        }

        Ok(node_id)
    }
}

fn blob_to_bitvec(b: &[u8]) -> BitVec {
    let words: Vec<usize> = b.chunks(size_of::<usize>())
        .map(|chunk| {
            let mut arr = [0u8; size_of::<usize>()];
            let len = chunk.len().min(size_of::<usize>());
            arr[..len].copy_from_slice(chunk);
            usize::from_le_bytes(arr)
        })
        .collect();
    BitVec::from_slice(&words)
}

fn vec_to_blob(v: &[f32]) -> Vec<u8> {
    v.iter()
        .flat_map(|f| f.to_le_bytes())
        .collect()
}

fn blob_to_vec(b: &[u8]) -> Option<Vec<f32>> {
    if b.len() % 4 != 0 {
        return None;
    }
    let chunks: Vec<[u8; 4]> = b
        .chunks_exact(4)
        .map(|c| {
            let mut arr = [0u8; 4];
            arr.copy_from_slice(c);
            arr
        })
        .collect();
    Some(chunks.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum();
    let norm_b: f32 = b.iter().map(|x| x * x).sum();
    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom < f32::EPSILON {
        1.0
    } else {
        1.0 - (dot / denom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_empty_embeddings(count: usize, dims: usize) -> Vec<(String, Vec<f32>)> {
        (0..count)
            .map(|i| {
                let mut v = Vec::with_capacity(dims);
                for j in 0..dims {
                    v.push(((i * dims + j) as f32) / (count * dims) as f32);
                }
                (format!("node_{i}"), v)
            })
            .collect()
    }

    #[test]
    fn test_init_schema() {
        let lib = Library::open_in_memory().expect("in-memory db");
        assert!(lib.node_count().is_ok());
    }

    #[test]
    fn test_insert_and_get_node() {
        let lib = Library::open_in_memory().expect("in-memory db");
        let node = ContextNode {
            id: None,
            name: "test_node".into(),
            source: "full_source_text".into(),
            lod: vec!["summary".into(), "brief".into()],
            embedding: None,
            capabilities: None,
        };
        let node_id = lib.insert_node(&node).expect("insert");
        assert!(node_id.as_int() > 0);

        let found = lib
            .find_node_by_name("test_node")
            .expect("find")
            .expect("should exist");
        assert_eq!(found.as_int(), node_id.as_int());
    }

    #[test]
    fn test_get_node_roundtrip() {
        let lib = Library::open_in_memory().expect("in-memory db");
        let emb: Vec<f32> = vec![0.1, 0.2, 0.3];
        let node = ContextNode {
            id: None,
            name: "roundtrip_node".into(),
            source: "source_text".into(),
            lod: vec!["full".into(), "summary".into(), "brief".into()],
            embedding: Some(emb.clone()),
            capabilities: None,
        };
        let node_id = lib.insert_node(&node).expect("insert");

        let gotten = lib
            .get_node(node_id)
            .expect("get")
            .expect("should exist");
        assert_eq!(gotten.name.as_str(), "roundtrip_node");
        assert_eq!(gotten.source, "source_text");
        assert_eq!(gotten.lod.len(), 3);
        if let Some(got_emb) = &gotten.embedding {
            assert!((got_emb[0] - 0.1).abs() < 1e-6);
        } else {
            panic!("embedding should exist");
        }
    }

    #[test]
    fn test_knn_search() {
        let lib = Library::open_in_memory().expect("in-memory db");
        let items = make_empty_embeddings(10, 4);

        for (name, emb) in &items {
            let node = ContextNode {
                id: None,
                name: name.as_str().into(),
                source: "source".into(),
                lod: vec![],
            embedding: Some(emb.clone()),
            capabilities: None,
        };
            lib.insert_node(&node).expect("insert");
        }

        let query: Vec<f32> = vec![0.0, 0.1, 0.2, 0.3];
        let hits = lib.knn_search(&query, 3, None).expect("knn search");
        assert_eq!(hits.len(), 3);
        assert!(hits[0].distance <= hits[1].distance);
    }

    #[test]
    fn test_traverse_from() {
        let lib = Library::open_in_memory().expect("in-memory db");

        let root = ContextNode {
            id: None,
            name: "root".into(),
            source: "root".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let root_id = lib.insert_node(&root).expect("insert");

        let child = ContextNode {
            id: None,
            name: "child".into(),
            source: "child".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let child_id = lib.insert_node(&child).expect("insert");

        let grandchild = ContextNode {
            id: None,
            name: "grandchild".into(),
            source: "grandchild".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let grandchild_id = lib.insert_node(&grandchild).expect("insert");

        lib.insert_edge(root_id, child_id, "depends", 1.0).expect("edge");
        lib.insert_edge(child_id, grandchild_id, "depends", 1.0)
            .expect("edge");

        let nodes = lib.traverse_from(root_id, 2).expect("traverse");
        assert_eq!(nodes.len(), 3);
    }

    #[test]
    fn test_insert_wasm_tool() {
        let lib = Library::open_in_memory().expect("in-memory db");
        let tool = WasmTool {
            name: "tokenizer".into(),
            path: "/bin/tokenizer.wasm".into(),
            capabilities: vec!["tokenize".into(), "split".into()],
        };
        lib.insert_wasm_tool(&tool).expect("insert");

        let found = lib
            .find_wasm_tools_by_capability("tokenize")
            .expect("find");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name.as_str(), "tokenizer");

        let not_found = lib
            .find_wasm_tools_by_capability("embed")
            .expect("find");
        assert!(not_found.is_empty());
    }

    #[test]
    fn test_embedding_cache() {
        let lib = Library::open_in_memory().expect("in-memory db");
        let emb: Vec<f32> = vec![0.5, 0.5, 0.5];
        lib.cache_embedding("hash123", "test query", &emb)
            .expect("cache");

        let cached = lib
            .get_cached_embedding("hash123")
            .expect("get cached");
        assert!(cached.is_some());
        let cached = cached.unwrap();
        assert!((cached[0] - 0.5).abs() < 1e-6);
    }

    #[test]
    fn test_knn_search_with_capability_filter() {
        let lib = Library::open_in_memory().expect("in-memory db");

        let node_a = ContextNode {
            id: None,
            name: "node_a".into(),
            source: "a".into(),
            lod: vec![],
            embedding: Some(vec![0.1, 0.2, 0.3, 0.4]),
            capabilities: Some(vec![0b0001]), // capability 0
        };
        lib.insert_node(&node_a).expect("insert");

        let node_b = ContextNode {
            id: None,
            name: "node_b".into(),
            source: "b".into(),
            lod: vec![],
            embedding: Some(vec![0.5, 0.6, 0.7, 0.8]),
            capabilities: Some(vec![0b0010]), // capability 1
        };
        lib.insert_node(&node_b).expect("insert");

        let node_c = ContextNode {
            id: None,
            name: "node_c".into(),
            source: "c".into(),
            lod: vec![],
            embedding: Some(vec![0.9, 1.0, 1.1, 1.2]),
            capabilities: Some(vec![0b0100]), // capability 2
        };
        lib.insert_node(&node_c).expect("insert");

        let query = vec![0.0, 0.1, 0.2, 0.3];

        let mut filter_cap0 = BitVec::new();
        filter_cap0.resize(4, false);
        filter_cap0.set(0, true);
        let hits_cap0 = lib.knn_search(&query, 10, Some(&filter_cap0)).expect("knn");
        assert_eq!(hits_cap0.len(), 1);
        assert_eq!(hits_cap0[0].name.as_str(), "node_a");

        let mut filter_cap1 = BitVec::new();
        filter_cap1.resize(4, false);
        filter_cap1.set(1, true);
        let hits_cap1 = lib.knn_search(&query, 10, Some(&filter_cap1)).expect("knn");
        assert_eq!(hits_cap1.len(), 1);
        assert_eq!(hits_cap1[0].name.as_str(), "node_b");

        let mut filter_all = BitVec::new();
        filter_all.resize(4, false);
        filter_all.set(0, true);
        filter_all.set(1, true);
        filter_all.set(2, true);
        let hits_all = lib.knn_search(&query, 10, Some(&filter_all)).expect("knn");
        assert_eq!(hits_all.len(), 3);
    }

    #[test]
    fn test_blob_roundtrip() {
        let original: Vec<f32> = vec![1.0, 2.0, 3.0, 4.0];
        let blob = vec_to_blob(&original);
        let restored = blob_to_vec(&blob).expect("should decode");
        assert_eq!(original.len(), restored.len());
        for (a, b) in original.iter().zip(restored.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn test_is_a_duck_typing() {
        let lib = Library::open_in_memory().expect("db");
        let node = ContextNode {
            id: None,
            name: "alice".into(),
            source: "source".into(),
            lod: vec![],
            embedding: None,
            capabilities: None,
        };
        let node_id = lib.insert_node(&node).expect("insert");

        lib.insert_entity_type(node_id, "http://schema.org/Person").expect("insert type");
        lib.insert_entity_hierarchy("http://schema.org/Person", "http://schema.org/Thing")
            .expect("insert hierarchy");

        let is_person = lib.is_a(node_id, "http://schema.org/Person").expect("is_a");
        assert!(is_person, "node should be a Person");

        let is_thing = lib.is_a(node_id, "http://schema.org/Thing").expect("is_a");
        assert!(is_thing, "node should be a Thing via hierarchy");

        let is_place = lib.is_a(node_id, "http://schema.org/Place").expect("is_a");
        assert!(!is_place, "node should NOT be a Place");
    }

    #[test]
    fn test_hydration_pipeline() {
        use guidance_common::embeddings::NoopEmbedding;
        use std::sync::Arc;

        let lib = Library::open_in_memory().expect("db");
        let embedder = Arc::new(NoopEmbedding::new(4));
        let pipeline = HydrationPipeline::new(&lib, embedder);

        let mut node = ContextNode {
            id: None,
            name: "hydrate_test".into(),
            source: "test source".into(),
            lod: vec!["some text to embed".into()],
            embedding: None,
            capabilities: None,
        };
        let node_id = pipeline.insert_and_hydrate(&mut node).expect("hydrate");
        assert!(node_id.as_int() > 0, "should get a valid node ID");

        let stored = lib.get_node(node_id).expect("get").expect("should exist");
        // NoopEmbedding returns empty vec, but node should be stored
        assert_eq!(stored.name.as_str(), "hydrate_test");
    }

    #[test]
    fn test_insert_entity_hierarchy() {
        let lib = Library::open_in_memory().expect("db");
        lib.insert_entity_hierarchy("sub", "super").expect("insert");
        let conn = lib.conn.lock().unwrap();
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM entity_hierarchy WHERE subclass_iri = ?1 AND superclass_iri = ?2",
            params!["sub", "super"],
            |row| row.get(0),
        ).expect("query");
        assert_eq!(count, 1);
    }

    #[test]
    fn test_cosine_distance_identical() {
        let a: Vec<f32> = vec![1.0, 0.0, 0.0];
        let b: Vec<f32> = vec![1.0, 0.0, 0.0];
        let d = cosine_distance(&a, &b);
        assert!((d - 0.0).abs() < 1e-6, "identical vectors should have distance 0");
    }

    #[test]
    fn test_cosine_distance_orthogonal() {
        let a: Vec<f32> = vec![1.0, 0.0];
        let b: Vec<f32> = vec![0.0, 1.0];
        let d = cosine_distance(&a, &b);
        assert!((d - 1.0).abs() < 1e-6, "orthogonal vectors should have distance 1");
    }
}

