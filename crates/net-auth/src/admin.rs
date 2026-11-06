use subtle::ConstantTimeEq;

/// Operator token check; an empty configured token disables operator endpoints.
pub fn admin_token_ok(configured: &str, presented: &str) -> bool {
    !configured.is_empty()
        && configured.len() == presented.len()
        && configured
            .as_bytes()
            .ct_eq(presented.as_bytes())
            .unwrap_u8()
            == 1
}

#[cfg(test)]
mod tests {
    #[test]
    fn empty_disables() {
        assert!(!super::admin_token_ok("", ""));
        assert!(super::admin_token_ok("abc", "abc"));
        assert!(!super::admin_token_ok("abc", "abd"));
    }
}
