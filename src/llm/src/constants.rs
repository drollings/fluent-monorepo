pub const MAX_EMBEDDING_DIMENSIONS: usize = 4_096;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_expected() {
        assert_eq!(MAX_EMBEDDING_DIMENSIONS, 4_096);
    }
}
