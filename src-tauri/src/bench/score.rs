//! Scoring for the benchmark harness.
//!
//! Two families of metric:
//! - **Recognition accuracy** vs the `spoken` reference: word error rate (WER)
//!   and character error rate (CER) on normalized text (lowercased, punctuation
//!   stripped). Measures how well the ASR model heard the words.
//! - **Format accuracy** vs the `expected` reference: exact match plus a
//!   normalized character-similarity ratio. Measures how well the full pipeline
//!   (ASR + text rules) produced the intended formatted output.
//!
//! No heavyweight deps — a single generic Levenshtein powers everything.

use serde::Serialize;

/// Generic Levenshtein edit distance over any two slices of comparable items.
/// Used both at word granularity (WER) and character granularity (CER).
pub fn edit_distance<T: PartialEq>(a: &[T], b: &[T]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }

    // Two-row DP: prev/curr over the columns of `b`.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr: Vec<usize> = vec![0; b.len() + 1];

    for (i, ai) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bj) in b.iter().enumerate() {
            let cost = if ai == bj { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1) // deletion
                .min(curr[j] + 1) // insertion
                .min(prev[j] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b.len()]
}

/// Normalize text for recognition scoring: Unicode lowercase, drop everything
/// that is not a letter/number/whitespace, and collapse whitespace runs.
///
/// Umlauts and `ß` are letters, so they are preserved (never stripped as
/// punctuation) — only lowercased.
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last_was_space = true; // trims leading whitespace
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            for lc in ch.to_lowercase() {
                out.push(lc);
            }
            last_was_space = false;
        } else if ch.is_whitespace() && !last_was_space {
            out.push(' ');
            last_was_space = true;
        }
        // Any other char (punctuation/symbols) is dropped.
    }
    // Trim a single trailing space introduced by the collapse.
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

fn words(text: &str) -> Vec<String> {
    normalize(text).split(' ').map(str::to_string).collect()
}

fn norm_chars(text: &str) -> Vec<char> {
    normalize(text).chars().collect()
}

/// Recognition metrics for one transcription against its spoken reference.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Recognition {
    /// Word error rate: word edits / reference word count (0.0 = perfect).
    pub wer: f64,
    /// Character error rate on normalized text.
    pub cer: f64,
}

/// Word error rate of `hyp` against reference `reference` (both normalized).
pub fn wer(reference: &str, hyp: &str) -> f64 {
    let r = words(reference);
    let h = words(hyp);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Character error rate of `hyp` against reference `reference` (normalized).
pub fn cer(reference: &str, hyp: &str) -> f64 {
    let r = norm_chars(reference);
    let h = norm_chars(hyp);
    if r.is_empty() {
        return if h.is_empty() { 0.0 } else { 1.0 };
    }
    edit_distance(&r, &h) as f64 / r.len() as f64
}

/// Score a transcription against the spoken reference.
pub fn recognition(spoken: &str, hyp: &str) -> Recognition {
    Recognition {
        wer: wer(spoken, hyp),
        cer: cer(spoken, hyp),
    }
}

/// Format metrics for one pipeline output against its expected reference.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct Format {
    /// Byte-exact match after trimming surrounding whitespace.
    pub exact_match: bool,
    /// Normalized character-similarity ratio in `0.0..=1.0`
    /// (`1 - edit_distance / max_len`). Higher is closer to `expected`.
    pub format_accuracy: f64,
}

/// Score a pipeline output against the expected formatted reference.
pub fn format(expected: &str, hyp: &str) -> Format {
    let exact_match = expected.trim() == hyp.trim();

    let e = norm_chars(expected);
    let h = norm_chars(hyp);
    let max_len = e.len().max(h.len());
    let format_accuracy = if max_len == 0 {
        1.0
    } else {
        1.0 - edit_distance(&e, &h) as f64 / max_len as f64
    };

    Format {
        exact_match,
        format_accuracy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edit_distance_basics() {
        assert_eq!(edit_distance(b"kitten", b"sitting"), 3);
        assert_eq!(edit_distance::<u8>(b"", b"abc"), 3);
        assert_eq!(edit_distance(b"same", b"same"), 0);
    }

    #[test]
    fn wer_known_examples() {
        // Perfect.
        assert_eq!(wer("the quick brown fox", "the quick brown fox"), 0.0);
        // One substitution out of four words.
        assert_eq!(wer("the quick brown fox", "the quick brown cat"), 0.25);
        // One deletion out of four words.
        assert_eq!(wer("the quick brown fox", "the quick brown"), 0.25);
        // Punctuation and case are ignored by normalization.
        assert_eq!(wer("Hello, world!", "hello world"), 0.0);
    }

    #[test]
    fn cer_known_example() {
        // "abc" -> "abd": one char substitution over three reference chars.
        assert!((cer("abc", "abd") - 1.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn umlauts_are_preserved_not_stripped() {
        // Umlauts/ß survive normalization as ordinary letters, only lowercased.
        assert_eq!(normalize("Grüße, schön!"), "grüße schön");
        // A missing umlaut counts as exactly one character error, not a dropped
        // token — proving the umlaut is a scored character.
        let n = norm_chars("schön");
        assert_eq!(n.len(), 5);
        assert!((cer("schön", "schon") - 1.0 / 5.0).abs() < 1e-9);
    }

    #[test]
    fn format_exact_and_ratio() {
        let f = format("Guten Tag. Hallo?", "Guten Tag. Hallo?");
        assert!(f.exact_match);
        assert!((f.format_accuracy - 1.0).abs() < 1e-9);

        // Missing punctuation: still counts as an exact-match failure, but the
        // normalized similarity stays high because the words are identical.
        let f = format("Guten Tag. Hallo?", "guten tag hallo");
        assert!(!f.exact_match);
        assert!((f.format_accuracy - 1.0).abs() < 1e-9);

        // Completely different text scores low.
        let f = format("hallo welt", "xyz");
        assert!(f.format_accuracy < 0.3);
    }
}
