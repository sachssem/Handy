//! Word-level correction differ and the anti-poisoning gate pipeline
//! (fork feature: voice-control).
//!
//! Given the recognizer's `original` output and the text the user left in the
//! field after editing, [`extract_correction`] diffs the two at word
//! granularity, collapses adjacent delete/insert runs into substitution pairs,
//! and applies a chain of gates that reject anything that does not look like a
//! genuine single-word / short-phrase mishearing: reformulations, pure
//! insertions/deletions, case-only edits, unrelated substitutions and everyday
//! function words.
//!
//! The differ is pure and table-tested. Phase A ships it so its behaviour is
//! locked by tests; the live caller — the post-paste learning session — arrives
//! in Phase B, so the extraction API is unused in non-test builds until then.
#![allow(dead_code)]

use similar::{ChangeTag, TextDiff};

/// A correction candidate that survived the gate pipeline: the recognizer's
/// `misheard` span mapped to the user's `intended` replacement, original casing
/// preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub misheard: String,
    pub intended: String,
}

/// Maximum words on either side of a learnable substitution. Longer spans are
/// almost always reformulations, not mishearings.
const MAX_PHRASE_WORDS: usize = 3;

/// Maximum edit distance between the two spans relative to the longer one. A
/// clean mishearing differs in a few characters; an unrelated rewrite does not.
const MAX_RELATIVE_DISTANCE: f64 = 0.5;

/// Extract a single correction candidate from an ASR `original` and its edited
/// `corrected` form, or `None` if the edit is not a learnable mishearing.
pub fn extract_correction(original: &str, corrected: &str) -> Option<Candidate> {
    let candidate = single_substitution(original, corrected)?;
    gate(candidate)
}

/// A contiguous change run collapsed from the word diff.
struct Run {
    deleted: Vec<String>,
    inserted: Vec<String>,
}

impl Run {
    fn new() -> Self {
        Run {
            deleted: Vec::new(),
            inserted: Vec::new(),
        }
    }

    fn is_empty(&self) -> bool {
        self.deleted.is_empty() && self.inserted.is_empty()
    }
}

/// Diff `original` vs `corrected` at word granularity and, iff the edit is a
/// single delete+insert run (one substitution, nothing else), return its
/// `(misheard, intended)` spans. Pure inserts, pure deletes and multi-region
/// edits return `None`.
///
/// Any `Equal` token — word or whitespace — closes the current run, so two
/// disjoint word swaps in one utterance become two runs and are rejected below;
/// a multi-word span (e.g. `New York → NYC`, whose internal space is itself
/// deleted) stays a single run.
fn single_substitution(original: &str, corrected: &str) -> Option<Candidate> {
    let diff = TextDiff::from_unicode_words(original, corrected);

    let mut runs: Vec<Run> = Vec::new();
    let mut current = Run::new();

    for change in diff.iter_all_changes() {
        let value = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                if !current.is_empty() {
                    runs.push(std::mem::replace(&mut current, Run::new()));
                }
            }
            ChangeTag::Delete if is_word(value) => current.deleted.push(value.trim().to_string()),
            ChangeTag::Insert if is_word(value) => current.inserted.push(value.trim().to_string()),
            // Whitespace/punctuation tokens inside a run carry no signal.
            _ => {}
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }

    let [run] = runs.as_slice() else {
        return None;
    };
    // A real substitution has content on both sides; pure insert/delete does not.
    if run.deleted.is_empty() || run.inserted.is_empty() {
        return None;
    }

    Some(Candidate {
        misheard: run.deleted.join(" "),
        intended: run.inserted.join(" "),
    })
}

/// Run the ordered anti-poisoning gates over a raw substitution candidate.
fn gate(candidate: Candidate) -> Option<Candidate> {
    let misheard = normalize(&candidate.misheard);
    let intended = normalize(&candidate.intended);

    // Both sides must carry content after normalization.
    if misheard.is_empty() || intended.is_empty() {
        return None;
    }
    // Case-/punctuation-only edits are not mishearings. Note `normalize` folds
    // neither `ß`/`ss` nor diacritics, so `Muller → Müller` still differs here.
    if misheard == intended {
        return None;
    }
    // Bound the phrase length; longer spans are reformulations.
    if word_count(&misheard) > MAX_PHRASE_WORDS || word_count(&intended) > MAX_PHRASE_WORDS {
        return None;
    }
    // Reject spans too far apart to be a mishearing (an unrelated rewrite).
    let distance = strsim::levenshtein(&misheard, &intended);
    let span = misheard.chars().count().max(intended.chars().count());
    if span == 0 || (distance as f64) / (span as f64) > MAX_RELATIVE_DISTANCE {
        return None;
    }
    // Skip everyday function words — we want to learn names/jargon, not "and".
    if is_common_word(&misheard) {
        return None;
    }

    Some(candidate)
}

/// Whether a diff token contains any word character (so whitespace/punctuation
/// tokens can be skipped when collapsing runs).
fn is_word(token: &str) -> bool {
    token.chars().any(char::is_alphanumeric)
}

/// Lowercase and strip non-alphanumeric characters for gate comparisons.
///
/// Deliberately does *not* fold `ß`/`ss` or strip diacritics, so `Muller →
/// Müller` and `Gruss → Gruß` remain learnable rather than looking like
/// case-only noise.
fn normalize(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn word_count(normalized: &str) -> usize {
    normalized.split_whitespace().count()
}

/// A minimal English + German function-word list. Phase A keeps this
/// deliberately tiny and explicit; richer frequency and phonetic gating arrive
/// in Phase D.
fn is_common_word(normalized: &str) -> bool {
    const COMMON: &[&str] = &[
        "the", "a", "an", "and", "or", "but", "to", "of", "in", "on", "is", "it", "i", "you", "he",
        "she", "we", "they", "this", "that", "der", "die", "das", "und", "oder", "ist", "ein",
        "eine", "ich", "du", "zu", "den", "dem", "nicht", "auch", "mit",
    ];
    COMMON.contains(&normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract(original: &str, corrected: &str) -> Option<Candidate> {
        extract_correction(original, corrected)
    }

    fn candidate(misheard: &str, intended: &str) -> Option<Candidate> {
        Some(Candidate {
            misheard: misheard.to_string(),
            intended: intended.to_string(),
        })
    }

    #[test]
    fn clean_substitution_is_learned() {
        assert_eq!(
            extract("send it to Jon", "send it to John"),
            candidate("Jon", "John")
        );
    }

    #[test]
    fn pure_insert_is_rejected() {
        assert_eq!(extract("hello world", "hello there world"), None);
    }

    #[test]
    fn pure_delete_is_rejected() {
        assert_eq!(extract("hello there world", "hello world"), None);
    }

    #[test]
    fn case_only_edit_is_rejected() {
        assert_eq!(extract("i live in münchen", "i live in München"), None);
    }

    #[test]
    fn unrelated_single_rewrite_is_rejected() {
        // One region, but the words are unrelated (distance too large).
        assert_eq!(extract("meet in Munchen", "meet in Barcelona"), None);
    }

    #[test]
    fn multi_region_rewrite_is_rejected() {
        // Two disjoint word swaps in one utterance → reformulation, not a fix.
        assert_eq!(extract("the quick brown fox", "the slow brown cat"), None);
    }

    #[test]
    fn german_umlaut_is_learned() {
        assert_eq!(
            extract("Ich war in Munchen", "Ich war in München"),
            candidate("Munchen", "München")
        );
    }

    #[test]
    fn eszett_is_learned_and_preserved() {
        // `ss → ß` is a real fix (not folded away by normalization); the stored
        // `intended` keeps the ß intact.
        assert_eq!(
            extract("die Strasse ist lang", "die Straße ist lang"),
            candidate("Strasse", "Straße")
        );
    }

    #[test]
    fn common_word_is_rejected() {
        // `der → den` passes the distance gate but is an everyday function word.
        assert_eq!(extract("ich gehe zu der Tür", "ich gehe zu den Tür"), None);
    }
}
