//! Word lists behind the learning gates (fork feature: voice-control).
//!
//! The learner wants new vocabulary — names, jargon, product spellings — not
//! grammar. Two compact lists make that distinction cheap and offline:
//!
//! - `data/common_de.txt` / `data/common_en.txt`: the 10 000 most frequent
//!   words per language (hermitdave/FrequencyWords, OpenSubtitles 2018,
//!   CC BY-SA 4.0 — attribution and the list of changes sit in each file's
//!   header). A pair whose *both* sides consist only of such words is an
//!   everyday-word swap (`Montag → Sonntag`, `Tuesday → Thursday`), not a
//!   mishearing worth remembering.
//! - `data/names.txt`: common personal first names (BSD `propernames`), so a
//!   name fix like `Jon → John` survives even though both spellings are
//!   frequent subtitle tokens.
//!
//! German and English are always consulted together: users code-switch, and a
//! language-tagged list would let an English everyday word through on a German
//! transcription.

use std::collections::HashSet;
use std::sync::OnceLock;

const COMMON_DE: &str = include_str!("data/common_de.txt");
const COMMON_EN: &str = include_str!("data/common_en.txt");
const NAMES: &str = include_str!("data/names.txt");

/// Parse a bundled list: one entry per line, `#` comments and blanks skipped,
/// entries lowercased.
fn parse(sources: &[&'static str]) -> HashSet<String> {
    sources
        .iter()
        .flat_map(|source| source.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_lowercase)
        .collect()
}

fn common_words() -> &'static HashSet<String> {
    static COMMON: OnceLock<HashSet<String>> = OnceLock::new();
    COMMON.get_or_init(|| parse(&[COMMON_DE, COMMON_EN]))
}

fn names() -> &'static HashSet<String> {
    static NAMES_SET: OnceLock<HashSet<String>> = OnceLock::new();
    NAMES_SET.get_or_init(|| parse(&[NAMES]))
}

/// Whether a normalized (lowercase, punctuation-free) word is among the most
/// frequent German or English words.
pub(crate) fn is_common(normalized_word: &str) -> bool {
    common_words().contains(normalized_word)
}

/// Whether a word is a known personal first name, ignoring case.
pub(crate) fn is_name(word: &str) -> bool {
    names().contains(&word.to_lowercase())
}

/// Whether a normalized word is everyday vocabulary or a known first name.
pub(crate) fn is_known(normalized_word: &str) -> bool {
    is_common(normalized_word) || is_name(normalized_word)
}

/// Word endings that only inflect a stem: German case/number/verb endings and
/// English plural/tense/comparative endings. Two words sharing a stem and
/// differing only by these are grammar, not vocabulary (`einen → einem`,
/// `Termin → Termine`, `your → you're`). `ß`/`ss` and other spelling endings
/// are deliberately absent so orthographic fixes (`Sachse → Sachße`) pass.
const INFLECTION_ENDINGS: &[&str] = &[
    "", "e", "en", "em", "er", "es", "n", "s", "m", "r", "t", "st", "te", "ten", "ter", "tes",
    "et", "est", "ern", "ens", "nen", "d", "ed", "ing", "ly", "re", "ve", "ll",
];

pub(crate) fn is_inflection_ending(suffix: &str) -> bool {
    INFLECTION_ENDINGS.contains(&suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_load_and_classify() {
        assert!(is_common("montag"));
        assert!(is_common("thursday"));
        assert!(is_common("einem"));
        assert!(!is_common("kubernetes"));
        assert!(!is_common("sachße"));
        assert!(!is_common("pricepower"));
        assert!(is_name("john"));
        assert!(!is_name("montag"));
        // Header comments never leak into the sets.
        assert!(!is_common("#"));
    }

    #[test]
    fn names_are_case_insensitive_and_known_words_include_names() {
        for name in ["marc", "Marc", "MARC", "John", "Hermann"] {
            assert!(is_name(name), "{name}");
        }
        for word in ["milch", "brot", "eier", "house", "marc", "john", "hermann"] {
            assert!(is_known(word), "{word}");
        }
        for typo in ["mlch", "broti", "eir", "hosue"] {
            assert!(!is_known(typo), "{typo}");
        }
    }
}
