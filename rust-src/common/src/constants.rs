pub const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;
pub const MAX_MCP_REQUEST_SIZE: usize = 10 * 1024 * 1024;
pub const MAX_KNN_CANDIDATES: usize = 100_000;
pub const MAX_EMBEDDING_DIMENSIONS: usize = 4_096;
pub const MAX_JSON_DEPTH: usize = 100;
pub const MAX_WASM_HOST_CALLS: u32 = 10_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_zig_values() {
        assert_eq!(MAX_FILE_SIZE, 100 * 1024 * 1024);
        assert_eq!(MAX_MCP_REQUEST_SIZE, 10 * 1024 * 1024);
        assert_eq!(MAX_KNN_CANDIDATES, 100_000);
        assert_eq!(MAX_EMBEDDING_DIMENSIONS, 4_096);
        assert_eq!(MAX_JSON_DEPTH, 100);
        assert_eq!(MAX_WASM_HOST_CALLS, 10_000);
    }
}
