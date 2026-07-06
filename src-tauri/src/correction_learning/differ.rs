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
//! The differ is pure and table-tested. Its live caller is the post-paste
//! learning session, which only exists on macOS; on other platforms the
//! extraction API is unused outside tests.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use rphonetic::{Cologne, DoubleMetaphone, Encoder};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};
use specta::Type;

/// A correction candidate that survived the gate pipeline: the recognizer's
/// `misheard` span mapped to the user's `intended` replacement, original casing
/// preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub misheard: String,
    pub intended: String,
}

/// How aggressively the learning pipeline accepts a candidate. Maps to a
/// [`GateProfile`] of concrete thresholds. Default is [`Conservative`], the
/// safest option against dictionary poisoning (design doc risk #4).
///
/// [`Conservative`]: Aggressiveness::Conservative
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum Aggressiveness {
    /// Tightest distance bound, phonetic agreement required for borderline
    /// edits, everyday function words rejected.
    #[default]
    Conservative,
    /// The Phase A/B thresholds: a looser distance bound, phonetics advisory
    /// only, function words still rejected.
    Balanced,
    /// Loosest distance bound, no phonetic requirement, function words allowed.
    Aggressive,
}

/// The concrete gate thresholds derived from an [`Aggressiveness`] level.
#[derive(Debug, Clone, Copy)]
pub struct GateProfile {
    /// Maximum edit distance between the two spans relative to the longer one. A
    /// clean mishearing differs in a few characters; an unrelated rewrite does
    /// not.
    max_relative_distance: f64,
    /// Whether a borderline substitution additionally has to sound alike. Never
    /// hard-blocks a clearly-low-distance edit (see [`PHONETIC_FLOOR`]).
    require_phonetic: bool,
    /// Whether everyday function words are rejected (we want names/jargon).
    reject_common_words: bool,
}

impl GateProfile {
    /// The threshold table. Each level is strictly looser than the previous:
    /// Conservative ⊂ Balanced ⊂ Aggressive.
    pub fn for_aggressiveness(level: Aggressiveness) -> Self {
        match level {
            Aggressiveness::Conservative => GateProfile {
                max_relative_distance: 0.4,
                require_phonetic: true,
                reject_common_words: true,
            },
            Aggressiveness::Balanced => GateProfile {
                max_relative_distance: 0.5,
                require_phonetic: false,
                reject_common_words: true,
            },
            Aggressiveness::Aggressive => GateProfile {
                max_relative_distance: 0.7,
                require_phonetic: false,
                reject_common_words: false,
            },
        }
    }
}

/// Which phonetic algorithm the borderline gate uses. German gets Kölner
/// Phonetik (umlaut/`ß`-aware); everything else Double Metaphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneticLang {
    German,
    Other,
}

/// Maximum words on either side of a learnable substitution. Longer spans are
/// almost always reformulations, not mishearings.
const MAX_PHRASE_WORDS: usize = 3;

/// Below this relative edit distance a substitution is *so* close that the
/// phonetic gate never rejects it — phonetics only arbitrates the borderline
/// band above it (design doc: "Phonetik-Gate nie hart blockierend bei eindeutig
/// niedriger Levenshtein-Distanz"). This is what keeps `Muller → Müller` (a
/// one-char umlaut fix) learnable at every aggressiveness level.
const PHONETIC_FLOOR: f64 = 0.25;

/// Extract a single correction candidate from an ASR `original` and its edited
/// `corrected` form, or `None` if the edit is not a learnable mishearing. The
/// `profile` sets the gate thresholds and `lang` the phonetic algorithm.
pub fn extract_correction(
    original: &str,
    corrected: &str,
    profile: &GateProfile,
    lang: PhoneticLang,
) -> Option<Candidate> {
    let candidate = single_substitution(original, corrected)?;
    gate(candidate, profile, lang)
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
fn gate(candidate: Candidate, profile: &GateProfile, lang: PhoneticLang) -> Option<Candidate> {
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
    if span == 0 {
        return None;
    }
    let relative = (distance as f64) / (span as f64);
    if relative > profile.max_relative_distance {
        return None;
    }
    // Skip everyday function words — we want to learn names/jargon, not "and".
    if profile.reject_common_words && is_common_word(&misheard) {
        return None;
    }
    // Phonetic gate (borderline band only): a substitution close enough to be a
    // clear mishearing is never blocked (`relative <= PHONETIC_FLOOR`); above
    // that band the Conservative profile additionally requires the two spans to
    // sound alike, so an unrelated same-length rewrite is rejected.
    if profile.require_phonetic
        && relative > PHONETIC_FLOOR
        && !sounds_alike(&misheard, &intended, lang)
    {
        return None;
    }

    Some(candidate)
}

/// Whether two normalized spans are phonetic equivalents under `lang`. Multi-word
/// spans are compared word-by-word (all pairs must agree, and both sides must
/// have the same word count).
fn sounds_alike(misheard: &str, intended: &str, lang: PhoneticLang) -> bool {
    let misheard_words: Vec<&str> = misheard.split_whitespace().collect();
    let intended_words: Vec<&str> = intended.split_whitespace().collect();
    if misheard_words.len() != intended_words.len() {
        return false;
    }
    misheard_words
        .iter()
        .zip(&intended_words)
        .all(|(a, b)| word_sounds_alike(a, b, lang))
}

/// Phonetic equality of two single words. German uses Kölner Phonetik (encodes
/// umlauts and `ß`), everything else Double Metaphone (its primary code).
fn word_sounds_alike(a: &str, b: &str, lang: PhoneticLang) -> bool {
    match lang {
        PhoneticLang::German => Cologne.is_encoded_equals(a, b),
        PhoneticLang::Other => DoubleMetaphone::new(None).is_encoded_equals(a, b),
    }
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

/// A minimal English + German function-word list, kept deliberately tiny and
/// explicit. Phonetic gating (Phase D) narrows borderline substitutions further;
/// a fuller frequency list can follow if poisoning still slips through.
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

    /// Default extraction for the Phase A/B gate behaviour, which the
    /// [`Aggressiveness::Balanced`] profile reproduces exactly.
    fn extract(original: &str, corrected: &str) -> Option<Candidate> {
        extract_correction(
            original,
            corrected,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
        )
    }

    fn extract_with(
        original: &str,
        corrected: &str,
        level: Aggressiveness,
        lang: PhoneticLang,
    ) -> Option<Candidate> {
        extract_correction(
            original,
            corrected,
            &GateProfile::for_aggressiveness(level),
            lang,
        )
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

    // --- Phase D: aggressiveness profiles ---------------------------------

    /// Each row names a pair that should be learnable only at the given level or
    /// looser. `Muller → Müller` is a one-char umlaut fix (learnable everywhere);
    /// `Berger → Merten` sits at relative distance 0.5 (Balanced+); `Meier →
    /// Bauer` at 0.6 (Aggressive only). Phonetics is irrelevant here — the
    /// Conservative rejections all trip the distance bound first.
    #[test]
    fn aggressiveness_profiles_gate_by_distance() {
        struct Case {
            original: &'static str,
            corrected: &'static str,
            conservative: bool,
            balanced: bool,
            aggressive: bool,
        }
        let cases = [
            Case {
                original: "ich war in Muller",
                corrected: "ich war in Müller",
                conservative: true,
                balanced: true,
                aggressive: true,
            },
            Case {
                original: "ich bin Berger",
                corrected: "ich bin Merten",
                conservative: false,
                balanced: true,
                aggressive: true,
            },
            Case {
                original: "ich bin Meier",
                corrected: "ich bin Bauer",
                conservative: false,
                balanced: false,
                aggressive: true,
            },
        ];
        for case in cases {
            for (level, expected) in [
                (Aggressiveness::Conservative, case.conservative),
                (Aggressiveness::Balanced, case.balanced),
                (Aggressiveness::Aggressive, case.aggressive),
            ] {
                let learned =
                    extract_with(case.original, case.corrected, level, PhoneticLang::Other)
                        .is_some();
                assert_eq!(
                    learned, expected,
                    "{:?}: {} → {}",
                    level, case.original, case.corrected
                );
            }
        }
    }

    // --- Phase D: phonetic gate -------------------------------------------

    #[test]
    fn umlaut_fix_never_hard_blocked() {
        // `Muller → Müller` is below PHONETIC_FLOOR, so it passes even the
        // Conservative profile with the German phonetic gate active.
        assert_eq!(
            extract_with(
                "Ich war in Muller",
                "Ich war in Müller",
                Aggressiveness::Conservative,
                PhoneticLang::German,
            ),
            candidate("Muller", "Müller")
        );
    }

    #[test]
    fn german_homophone_passes_conservative_via_cologne() {
        // `Meier`/`Mayer` sit in the borderline band (distance > floor) but are
        // equal under Kölner Phonetik, so the Conservative phonetic gate lets
        // them through.
        assert_eq!(
            extract_with(
                "das ist Meier",
                "das ist Mayer",
                Aggressiveness::Conservative,
                PhoneticLang::German,
            ),
            candidate("Meier", "Mayer")
        );
    }

    #[test]
    fn english_homophone_passes_conservative_via_double_metaphone() {
        // `Steven`/`Stephen` are borderline by distance but identical under
        // Double Metaphone.
        assert_eq!(
            extract_with(
                "call Steven now",
                "call Stephen now",
                Aggressiveness::Conservative,
                PhoneticLang::Other,
            ),
            candidate("Steven", "Stephen")
        );
    }

    #[test]
    fn borderline_band_unrelated_rejected_under_conservative() {
        // `Meier → Meile`: within the Conservative distance bound and in the
        // borderline band, but phonetically distinct under Kölner Phonetik, so
        // Conservative rejects it while Balanced (phonetics advisory) accepts.
        assert_eq!(
            extract_with(
                "das ist Meier",
                "das ist Meile",
                Aggressiveness::Conservative,
                PhoneticLang::German,
            ),
            None
        );
        assert_eq!(
            extract_with(
                "das ist Meier",
                "das ist Meile",
                Aggressiveness::Balanced,
                PhoneticLang::German,
            ),
            candidate("Meier", "Meile")
        );
    }
}
