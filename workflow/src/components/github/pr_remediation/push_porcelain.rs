pub(super) fn parse_porcelain_z(output: &str) -> Vec<String> {
    let mut paths = Vec::new();
    let mut entries = output.split('\0');
    while let Some(entry) = entries.next() {
        // Entry format: "XY <path>". Need at least 3 bytes (2 status + 1 space);
        // entries with exactly 3 bytes have an empty path and are filtered below.
        if entry.len() < 3 {
            continue;
        }
        let code = &entry[..2];
        // Git porcelain v1 entries are formatted as "XY <path>" with
        // ASCII status bytes and separator, so index 3 is a UTF-8 boundary.
        let path = &entry[3..];
        if path.is_empty() {
            continue;
        }
        paths.push(path.to_string());
        if code.starts_with('R') || code.starts_with('C') {
            if let Some(next_path) = entries.next() {
                if !next_path.is_empty() {
                    paths.push(next_path.to_string());
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::parse_porcelain_z;

    #[test]
    fn parses_empty_output() {
        assert!(parse_porcelain_z("").is_empty());
    }

    #[test]
    fn parses_simple_modified_entry() {
        assert_eq!(parse_porcelain_z(" M src/lib.rs\0"), vec!["src/lib.rs"]);
    }

    #[test]
    fn parses_untracked_entry() {
        assert_eq!(parse_porcelain_z("?? untracked.rs\0"), vec!["untracked.rs"]);
    }

    #[test]
    fn skips_short_entries() {
        assert!(parse_porcelain_z("AB\0").is_empty());
    }

    #[test]
    fn skips_empty_path_entry() {
        assert!(parse_porcelain_z("XY \0").is_empty());
        assert!(parse_porcelain_z(" M \0").is_empty());
    }

    #[test]
    fn parses_rename_and_consumes_original_path() {
        assert_eq!(
            parse_porcelain_z("R  new.rs\0old.rs\0 M next.rs\0"),
            vec!["new.rs", "old.rs", "next.rs"]
        );
    }

    #[test]
    fn parses_copy_and_consumes_original_path() {
        assert_eq!(
            parse_porcelain_z("C  copy.rs\0source.rs\0"),
            vec!["copy.rs", "source.rs"]
        );
    }

    #[test]
    fn parses_multibyte_utf8_path() {
        assert_eq!(parse_porcelain_z(" M café.rs\0"), vec!["café.rs"]);
    }

    #[test]
    fn skips_empty_original_path_without_desynchronizing() {
        assert_eq!(
            parse_porcelain_z("R  renamed.rs\0\0 M next.rs\0"),
            vec!["renamed.rs", "next.rs"]
        );
    }
}
