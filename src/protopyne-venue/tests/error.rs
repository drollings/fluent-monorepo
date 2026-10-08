use super::*;

#[test]
fn retry_taxonomy_never_retries_a_refusal() {
    assert!(!VenueError::Refused("wide".to_owned()).is_retryable());
    assert!(!VenueError::Invalid("qty".to_owned()).is_retryable());
    assert!(!VenueError::Journal("io".to_owned()).is_retryable());
    assert!(!VenueError::Reconciliation("drift".to_owned()).is_retryable());
    assert!(!VenueError::Health("stale".to_owned()).is_retryable());
    assert!(VenueError::Transport("drop".to_owned()).is_retryable());
    assert!(VenueError::Server("500".to_owned()).is_retryable());
    assert!(VenueError::Timeout("slow".to_owned()).is_retryable());
}
