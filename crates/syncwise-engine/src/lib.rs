/// Normalize a filename for basic comparisons.
/// This is an initial implementation, not the final matching algorithm.
pub fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_whitespace_and_ignores_ascii_case() {
        assert_eq!(
            normalize_name("  Song.MP3  "),
            "song.mp3"
        );
    }
}