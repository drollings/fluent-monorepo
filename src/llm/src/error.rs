use crate::embeddings::EmbeddingError;

pub type EmbedError = EmbeddingError;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_error_display() {
        let err = EmbedError::NoApiKey;
        assert_eq!(format!("{err}"), "no API key provided");
    }
}
