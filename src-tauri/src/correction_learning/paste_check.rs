//! Post-paste read-back diagnosis.
//!
//! The learning session reads the focused field right after the paste. When
//! that first read does not contain the pasted text, the field shows something
//! else than what Handy sent: the "Cyrillic instead of German" reports were
//! traced to the insertion step, not to the recognizer. This module describes
//! what the field holds where the paste should be, so the journal can tell a
//! wrong-script insertion from a paste that was edited or never arrived.

use serde::Serialize;

/// Longest field excerpt kept in the journal.
const EXCERPT_CHARS: usize = 80;
/// Cap on the field region inspected for the dominant script.
const MAX_REGION_CHARS: usize = 2000;

/// What the field held instead of the pasted text on the first read.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PasteMismatch {
    /// Dominant script of the pasted text (`latin`, `cyrillic`, …, `none`).
    pub pasted_script: &'static str,
    /// Dominant script of the field text where the paste should be.
    pub field_script: &'static str,
    /// Characters in the inspected field region.
    pub field_chars: usize,
    /// Start of that region; cleared under the dictation's redaction rule.
    pub excerpt: String,
}

/// Describe the field region that should hold `pasted`: as many characters as
/// were pasted, ending at the caret (or at the field's end without a caret).
pub fn describe(field: &str, pasted: &str, caret: Option<usize>) -> PasteMismatch {
    let mut end = caret.unwrap_or(field.len()).min(field.len());
    while !field.is_char_boundary(end) {
        end -= 1;
    }
    let before = &field[..end];
    let want = pasted.trim().chars().count().min(MAX_REGION_CHARS);
    let start = before
        .char_indices()
        .rev()
        .nth(want.saturating_sub(1))
        .map_or(0, |(index, _)| index);
    let region = &before[start..];
    let excerpt: String = region
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(EXCERPT_CHARS)
        .collect();
    PasteMismatch {
        pasted_script: dominant_script(pasted),
        field_script: dominant_script(region),
        field_chars: region.chars().count(),
        excerpt,
    }
}

/// The script most letters of `text` belong to, `none` without letters.
pub fn dominant_script(text: &str) -> &'static str {
    const SCRIPTS: [&str; 9] = [
        "latin", "cyrillic", "greek", "han", "kana", "hangul", "arabic", "hebrew", "other",
    ];
    let mut counts = [0usize; SCRIPTS.len()];
    for c in text.chars().filter(|c| c.is_alphabetic()) {
        let index = match c as u32 {
            0x0041..=0x024F | 0x1E00..=0x1EFF => 0,
            0x0400..=0x052F | 0x1C80..=0x1C8F | 0x2DE0..=0x2DFF | 0xA640..=0xA69F => 1,
            0x0370..=0x03FF | 0x1F00..=0x1FFF => 2,
            0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x2FA1F => 3,
            0x3040..=0x30FF | 0x31F0..=0x31FF => 4,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => 5,
            0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF => 6,
            0x0590..=0x05FF => 7,
            _ => 8,
        };
        counts[index] += 1;
    }
    counts
        .iter()
        .enumerate()
        .filter(|(_, count)| **count > 0)
        // Ties go to the earlier entry, so the result is deterministic.
        .max_by_key(|(index, count)| (**count, SCRIPTS.len() - index))
        .map_or("none", |(index, _)| SCRIPTS[index])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_dominant_script() {
        assert_eq!(dominant_script("Prüfe das mal, bitte."), "latin");
        assert_eq!(dominant_script("Проверь это, please"), "cyrillic");
        assert_eq!(dominant_script("こんにちは"), "kana");
        assert_eq!(dominant_script("12 34 …"), "none");
    }

    #[test]
    fn describes_the_region_before_the_caret() {
        let field = "Notiz: Проверь это пожалуйста";
        let pasted = "Prüfe das bitte einmal";
        let mismatch = describe(field, pasted, None);
        assert_eq!(mismatch.pasted_script, "latin");
        assert_eq!(mismatch.field_script, "cyrillic");
        assert_eq!(mismatch.field_chars, pasted.chars().count());
        assert!(mismatch.excerpt.ends_with("пожалуйста"));
    }

    #[test]
    fn caret_inside_a_multibyte_char_and_short_fields_are_safe() {
        let field = "äö";
        // Byte 1 is inside `ä`.
        let mismatch = describe(field, "a long pasted sentence", Some(1));
        assert_eq!(mismatch.field_chars, 0);
        assert_eq!(mismatch.field_script, "none");
        let mismatch = describe(field, "a long pasted sentence", Some(99));
        assert_eq!(mismatch.excerpt, "äö");
    }

    #[test]
    fn excerpt_is_capped_and_whitespace_compacted() {
        let field = format!("a\n\n b {}", "x".repeat(200));
        let pasted = "y".repeat(300);
        let mismatch = describe(&field, &pasted, None);
        assert_eq!(mismatch.excerpt.chars().count(), EXCERPT_CHARS);
        assert!(mismatch.excerpt.starts_with("a b x"));
    }
}
