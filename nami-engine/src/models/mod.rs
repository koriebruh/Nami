pub mod heartbeat;
pub mod incident;
pub mod monitor;
pub mod project;
pub mod source;
pub mod token;
pub mod user;

/// Name rule shared by monitors and heartbeats: trimmed, 1-200 characters,
/// no control characters (names end up in alert email subjects) and none of
/// the invisible or text-direction characters that let a name render as
/// something it is not (see `is_invisible_or_bidi_char`).
pub fn validate_name(raw: &str) -> Result<String, String> {
    // Checked on the raw input, before trimming: U+2028 and U+2029 are
    // whitespace, so `trim` would otherwise drop them at the edges instead
    // of rejecting them.
    if raw.chars().any(is_invisible_or_bidi_char) {
        return Err("name must not contain invisible or text-direction characters".into());
    }
    let name = raw.trim();
    if name.is_empty() {
        return Err("name is required".into());
    }
    if name.chars().count() > 200 {
        return Err("name must be at most 200 characters".into());
    }
    if name.chars().any(char::is_control) {
        return Err("name must not contain control characters".into());
    }
    Ok(name.to_string())
}

/// Format and separator characters that change how text renders, or hide in
/// it, without being visible themselves: the soft hyphen, the Arabic letter
/// mark, the Mongolian vowel separator, zero-width spaces and joiners,
/// direction marks, line and paragraph separators, bidirectional embeddings,
/// overrides and isolates, the word joiner and invisible operators, and the
/// byte order mark.
fn is_invisible_or_bidi_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{2069}'
            | '\u{FEFF}'
    )
}

#[cfg(test)]
mod tests {
    use super::validate_name;

    const INVISIBLE: &str = "name must not contain invisible or text-direction characters";

    /// Every character `validate_name` must reject: U+00AD, U+061C, U+180E,
    /// U+200B..=U+200F, U+2028..=U+202E, U+2060..=U+2064, U+2066..=U+2069
    /// and U+FEFF.
    fn invisible_or_bidi_chars() -> Vec<char> {
        let mut chars = vec!['\u{00AD}', '\u{061C}', '\u{180E}', '\u{FEFF}'];
        for range in [
            0x200B..=0x200F,
            0x2028..=0x202E,
            0x2060..=0x2064,
            0x2066..=0x2069,
        ] {
            chars.extend(range.map(|code| char::from_u32(code).unwrap()));
        }
        chars
    }

    #[test]
    fn trims_surrounding_whitespace() {
        assert_eq!(validate_name("  API \t").unwrap(), "API");
    }

    #[test]
    fn rejects_empty_and_whitespace_only_names() {
        for raw in ["", "   ", "\t\n"] {
            assert_eq!(
                validate_name(raw).unwrap_err(),
                "name is required",
                "{raw:?}"
            );
        }
    }

    #[test]
    fn limits_the_length_to_200_characters_not_bytes() {
        assert!(validate_name(&"x".repeat(200)).is_ok());
        assert!(
            validate_name(&"\u{e9}".repeat(200)).is_ok(),
            "200 two-byte characters"
        );
        for raw in ["x".repeat(201), "\u{e9}".repeat(201)] {
            assert_eq!(
                validate_name(&raw).unwrap_err(),
                "name must be at most 200 characters"
            );
        }
    }

    #[test]
    fn rejects_control_characters() {
        for c in ['\0', '\t', '\n', '\r', '\u{1b}', '\u{7f}', '\u{85}'] {
            assert_eq!(
                validate_name(&format!("a{c}b")).unwrap_err(),
                "name must not contain control characters",
                "U+{:04X}",
                c as u32
            );
        }
    }

    #[test]
    fn rejects_invisible_and_text_direction_characters_anywhere_in_the_input() {
        let chars = invisible_or_bidi_chars();
        assert_eq!(chars.len(), 25, "the rejected set is 25 code points");
        for c in chars {
            // Also at the edges and alone: U+2028 / U+2029 are whitespace,
            // so a trim-first check would let them through there.
            for raw in [
                format!("a{c}b"),
                format!("{c}ab"),
                format!("ab{c}"),
                c.to_string(),
            ] {
                assert_eq!(
                    validate_name(&raw).unwrap_err(),
                    INVISIBLE,
                    "U+{:04X} in {raw:?}",
                    c as u32
                );
            }
        }
    }

    #[test]
    fn accepts_characters_next_to_the_rejected_ranges() {
        // Ordinary visible or space characters right below and above the
        // rejected ones, so a range that drifts wider is caught.
        for c in [
            '\u{00AC}', '\u{00AE}', '\u{061B}', '\u{061D}', '\u{180D}', '\u{180F}', '\u{200A}',
            '\u{2010}', '\u{2027}', '\u{202F}', '\u{205F}', '\u{FEFE}',
        ] {
            assert!(
                validate_name(&format!("a{c}b")).is_ok(),
                "U+{:04X}",
                c as u32
            );
        }
    }

    #[test]
    fn accepts_ordinary_unicode_names() {
        for raw in [
            "Caf\u{e9} \u{2615}",
            "\u{65e5}\u{672c}\u{8a9e} \u{30b5}\u{30fc}\u{30d0}\u{30fc}",
            "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}",
            "\u{1f680} Production",
            "API (eu-west) #1",
        ] {
            assert_eq!(validate_name(raw).unwrap(), raw);
        }
    }
}
