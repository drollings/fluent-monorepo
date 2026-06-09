pub const MAX_VALUE_LEN: usize = 128;
pub const MAX_FILE_SIZE: usize = 100 * 1024 * 1024;
pub const MAX_JSON_DEPTH: usize = 100;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_expected() {
        assert_eq!(MAX_VALUE_LEN, 128);
        assert_eq!(MAX_FILE_SIZE, 100 * 1024 * 1024);
        assert_eq!(MAX_JSON_DEPTH, 100);
    }
}
