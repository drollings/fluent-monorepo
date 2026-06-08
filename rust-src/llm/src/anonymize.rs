use std::sync::LazyLock;

use regex::Regex;

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}").unwrap());
static API_KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)(api[_-]?key|apikey|secret|token)\s*[=:]\s*[a-zA-Z0-9_-]{16,}"#).unwrap());
static IP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{1,3}\.\d{1,3}\.\d{1,3}\.\d{1,3}\b").unwrap());
static PHONE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\b\d{3}[-.]?\d{3}[-.]?\d{4}\b").unwrap());

pub fn anonymize(text: &str) -> String {
    let text = EMAIL_RE.replace_all(text, "[EMAIL]");
    let text = API_KEY_RE.replace_all(&text, "[REDACTED]");
    let text = IP_RE.replace_all(&text, "[IP_ADDRESS]");
    let text = PHONE_RE.replace_all(&text, "[PHONE]");
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anonymize_email() {
        let result = anonymize("Contact me at user@example.com");
        assert_eq!(result, "Contact me at [EMAIL]");
    }

    #[test]
    fn test_anonymize_api_key() {
        let result = anonymize("api_key=sk-1234567890abcdef1234567890abcdef");
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn test_anonymize_ip() {
        let result = anonymize("Server at 192.168.1.1 is active");
        assert_eq!(result, "Server at [IP_ADDRESS] is active");
    }

    #[test]
    fn test_anonymize_phone() {
        let result = anonymize("Call 555-123-4567 for info");
        assert!(result.contains("[PHONE]"));
    }

    #[test]
    fn test_anonymize_multiple() {
        let result = anonymize("user@test.com api_key=abcdefghijklmnop12345 from 10.0.0.1");
        assert!(result.contains("[EMAIL]"));
        assert!(result.contains("[REDACTED]"));
        assert!(result.contains("[IP_ADDRESS]"));
    }

    #[test]
    fn test_no_pii_unchanged() {
        let text = "This is a normal sentence with no PII.";
        let result = anonymize(text);
        assert_eq!(result, text);
    }
}
