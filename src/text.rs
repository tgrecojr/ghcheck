//! Output encoding for untrusted remote data.
//!
//! Everything this process writes to a terminal, to stderr, or to `--json`
//! originates from the GitHub API and is therefore attacker-influenced. This
//! module is the single encoder those egress paths share.

use unicode_general_category::{get_general_category, GeneralCategory};

/// Replacement character substituted for anything that must not reach a terminal.
const REPLACEMENT: char = '·';

/// Neutralize characters that a terminal would interpret rather than display.
///
/// Denies two Unicode categories, not a list of known-bad codepoints:
///
/// * **Cc** (`char::is_control`) — C0, DEL and C1, which carry ANSI escape
///   sequences.
/// * **Cf** — format characters. The bidi overrides and isolates
///   (U+202A-U+202E, U+2066-U+2069) reorder text *without terminating at a
///   cell boundary*, so an unterminated override in one column visually
///   rewrites the rest of the row; the zero-width range (U+200B-U+200F) hides
///   content outright.
///
/// Category membership comes from the Unicode tables rather than a local range
/// list so a codepoint assigned in a later Unicode release cannot silently
/// reopen this.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if (c.is_control() && c != '\t') || get_general_category(c) == GeneralCategory::Format {
                REPLACEMENT
            } else {
                c
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Characters in Unicode category Cf that a bidi-aware terminal acts on.
    /// Denying the category — rather than the individual codepoints seen in one
    /// payload — is what stops a newly-probed character reopening this.
    const CF_SAMPLES: &[(&str, char)] = &[
        ("U+202E RLO", '\u{202E}'),
        ("U+202D LRO", '\u{202D}'),
        ("U+202A LRE", '\u{202A}'),
        ("U+2066 LRI", '\u{2066}'),
        ("U+2067 RLI", '\u{2067}'),
        ("U+2069 PDI", '\u{2069}'),
        ("U+200B ZWSP", '\u{200B}'),
        ("U+200E LRM", '\u{200E}'),
        ("U+00AD SHY", '\u{00AD}'),
        ("U+061C ALM", '\u{061C}'),
        ("U+180E MVS", '\u{180E}'),
        ("U+FEFF BOM", '\u{FEFF}'),
        ("U+2060 WJ", '\u{2060}'),
        ("U+FFF9 IAA", '\u{FFF9}'),
        ("U+E0001 TAG", '\u{E0001}'),
    ];

    #[test]
    fn sanitize_neutralizes_every_format_character() {
        for (name, ch) in CF_SAMPLES {
            let out = sanitize(&format!("safe{ch}payload"));
            assert!(
                !out.contains(*ch),
                "{name} survived sanitize() and would reach the terminal: {out:?}"
            );
        }
    }

    #[test]
    fn sanitize_still_neutralizes_control_characters() {
        let out = sanitize("safe\u{1b}[31mattacker\u{7}");
        assert!(!out.contains('\u{1b}'), "ESC survived: {out:?}");
        assert!(!out.contains('\u{7}'), "BEL survived: {out:?}");
    }

    #[test]
    fn sanitize_preserves_ordinary_text() {
        // The encoder must not mangle legitimate content, or it would corrupt
        // honest PR titles.
        let s = "Bump lodash 4.17.21 — Café 日本語 🎉";
        assert_eq!(sanitize(s), s);
    }

    #[test]
    fn sanitize_makes_an_unterminated_override_impossible() {
        // The bleed in VULN-003 came from a truncated title stranding an RLO
        // with no terminating PDF. If no override survives encoding, the
        // stranding case cannot arise regardless of where truncation cuts.
        let title = format!("{}{}", '\u{202E}', "a".repeat(200));
        assert!(
            !sanitize(&title).contains('\u{202E}'),
            "RLO survived: would bleed into adjacent cells"
        );
    }
}
