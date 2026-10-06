use std::sync::OnceLock;

use regex::Regex;

fn patterns() -> &'static [Regex; 3] {
    static PATTERNS: OnceLock<[Regex; 3]> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            Regex::new(r"(?i)(https?://)[^/\s?#@]+@").expect("constant URL regex"),
            Regex::new(r#"(?i)\b(token|password|passwd|secret|api[_-]?key|access[_-]?token|refresh[_-]?token|authorization|auth|credential|credentials)\b(\s*[:=]\s*)(?:\"[^\"]*\"|'[^']*'|Bearer\s+[A-Za-z0-9._~+/=-]+|[^&,\s}]+)"#)
                .expect("constant credential-field regex"),
            Regex::new(r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]+").expect("constant bearer regex"),
        ]
    })
}

/// Redact common secrets before text is sent to logs or diagnostics.
pub fn redact(input: &str) -> String {
    let patterns = patterns();
    let url_safe = patterns[0].replace_all(input, "$1[REDACTED]@");
    let fields_safe = patterns[1].replace_all(&url_safe, "$1$2\"[REDACTED]\"");
    patterns[2]
        .replace_all(&fields_safe, "Bearer [REDACTED]")
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::redact;

    #[test]
    fn redacts_tokens_passwords_authorization_and_url_credentials() {
        let input = r#"GET https://alice:correct-horse@example.test/a?access_token=abc123 password="letmein" Authorization: Bearer eyJhbGciOiJIUzI1NiJ9"#;
        let safe = redact(input);
        for secret in [
            "alice:correct-horse",
            "abc123",
            "letmein",
            "eyJhbGciOiJIUzI1NiJ9",
        ] {
            assert!(!safe.contains(secret), "leaked {secret:?} in {safe:?}");
        }
        assert!(safe.contains("[REDACTED]"));
    }

    #[test]
    fn leaves_ordinary_text_unchanged() {
        assert_eq!(
            redact("fetch complete: package_count=3"),
            "fetch complete: package_count=3"
        );
    }
}
