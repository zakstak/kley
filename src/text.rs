pub fn truncate_with_suffix(input: &str, max_chars: usize, suffix: &str) -> String {
    if max_chars == 0 {
        return suffix.to_string();
    }

    let taken: String = input.chars().take(max_chars).collect();
    if taken.len() == input.len() {
        return input.to_string();
    }

    format!("{taken}{suffix}")
}

pub fn truncate_with_ascii_ellipsis(input: &str, max_chars: usize) -> String {
    truncate_with_suffix(input, max_chars, "...")
}

pub fn truncate_with_unicode_ellipsis(input: &str, max_chars: usize) -> String {
    truncate_with_suffix(input, max_chars, "…")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_with_suffix_preserves_utf8_boundaries() {
        assert_eq!(truncate_with_suffix("🦀 test", 1, "..."), "🦀...");
    }

    #[test]
    fn truncate_with_ascii_ellipsis_uses_suffix_only_when_needed() {
        assert_eq!(truncate_with_ascii_ellipsis("hello", 3), "hel...");
        assert_eq!(truncate_with_ascii_ellipsis("hello", 5), "hello");
    }

    #[test]
    fn truncate_with_unicode_ellipsis_uses_suffix_only_when_needed() {
        assert_eq!(truncate_with_unicode_ellipsis("🦀 test", 1), "🦀…");
    }

    #[test]
    fn truncate_with_suffix_handles_zero_max() {
        assert_eq!(truncate_with_suffix("hello", 0, "..."), "...");
    }
}
