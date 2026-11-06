//! Keyword-honest notes search over the store (FTS, no embeddings).
//!
//! Queries tokenize with the shared matchers; notes rank by token
//! overlap. Zero-dependency honesty: a note matches only when its own
//! words match, never by vector resemblance.

use fluent_db::error::DbError;
use fluent_db::store::SqliteStore;

/// One ranked search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoteHit {
    /// Note id.
    pub id: String,
    /// Revision at read time.
    pub rev: i64,
    /// Matched token count.
    pub score: usize,
}

/// Save (or replace) note `id` at the next revision.
pub fn save_note(store: &SqliteStore, id: &str, body: &str) -> Result<i64, DbError> {
    let next: i64 = store
        .query_row(
            "SELECT rev FROM notes WHERE id = ?1",
            &[&id as &dyn rusqlite::ToSql],
            |row| row.get::<_, i64>(0),
        )?
        .map_or(1, |rev| rev + 1);
    store.execute(
        "INSERT INTO notes (id, rev, body) VALUES (?1, ?2, ?3) \
         ON CONFLICT(id) DO UPDATE SET rev=excluded.rev, body=excluded.body",
        &[
            &id as &dyn rusqlite::ToSql,
            &next as &dyn rusqlite::ToSql,
            &body as &dyn rusqlite::ToSql,
        ],
    )?;
    Ok(next)
}

/// Search notes for `query`, best overlap first, at most `limit` hits.
pub fn search_notes(
    store: &SqliteStore,
    query: &str,
    limit: usize,
) -> Result<Vec<NoteHit>, DbError> {
    let wanted = common_core::string::fts_match_tokens(query);
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<(String, i64, String)> =
        store.query_rows("SELECT id, rev, body FROM notes", &[], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
    let mut hits: Vec<NoteHit> = rows
        .into_iter()
        .filter_map(|(id, rev, body)| {
            let body_tokens = common_core::string::ascii_alnum_tokens(&body.to_lowercase());
            let score = wanted
                .iter()
                .filter(|token| body_tokens.iter().any(|word| word == *token))
                .count();
            if score > 0 {
                Some(NoteHit { id, rev, score })
            } else {
                None
            }
        })
        .collect();
    hits.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.id.cmp(&b.id)));
    hits.truncate(limit.max(1));
    Ok(hits)
}

/// Action items derive only from explicit user requests, never from
/// transcript prose: a planted instruction inside quoted meeting text
/// must not become an action item (the notes half of the red-team
/// corpus). Lines starting with `Action:` in `user_requests` are
/// items; transcript lines never are.
#[must_use]
pub fn extract_action_items(transcript: &str, user_requests: &[String]) -> Vec<String> {
    let _ = transcript;
    user_requests
        .iter()
        .filter_map(|request| {
            request
                .strip_prefix("Action:")
                .map(|item| item.trim().to_string())
                .filter(|item| !item.is_empty())
        })
        .collect()
}

#[cfg(test)]
#[path = "../tests/search.rs"]
mod tests;
