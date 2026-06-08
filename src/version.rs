//! Crate version surfaced as a runtime constant for both targets.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::VERSION;

    #[test]
    fn version_is_non_empty() {
        assert!(!VERSION.is_empty());
    }

    #[test]
    fn version_parses_as_semver_triple() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3, "expected MAJOR.MINOR.PATCH, got {VERSION}");
        for part in parts {
            assert!(
                part.chars().all(|c| c.is_ascii_digit()),
                "non-numeric semver component in {VERSION}"
            );
        }
    }
}
