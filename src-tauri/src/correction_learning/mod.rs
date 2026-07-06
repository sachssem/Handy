//! Auto-learned corrections (fork feature: voice-control).
//!
//! A second, *exact* dictionary that complements Whisper/Parakeet's fuzzy
//! output: pairs of `misheard → intended` text substituted deterministically at
//! transcription time, word-boundary exact and longest-phrase first (VoiceInk-
//! style word replacements). Unlike `custom_words` (fuzzy, single tokens) it
//! rewrites exactly what the recognizer keeps getting wrong.
//!
//! The long-term feature learns these pairs on its own: after a transcription is
//! pasted, a macOS Accessibility read of the focused field is diffed against the
//! ASR output and a genuine mishearing is captured automatically. That loop is
//! built in phases:
//!
//! - **Phase A (this commit)** — the storage model ([`store`]), the
//!   deterministic apply stage ([`apply_learned`]) wired into the transcription
//!   funnel, the correction differ + anti-poisoning gates ([`differ`], locked by
//!   tests), and a review UI for manual add / enable / delete. Immediately
//!   useful as a hand-curated exact dictionary; no Accessibility code yet.
//! - **Phase B** — the macOS focused-field reader and the post-paste learning
//!   session that feed [`differ`] live.
//! - **Phase C** — the "learned X → Y" toast with undo.
//! - **Phase D** — the aggressiveness setting, the phonetic borderline gate
//!   (Double Metaphone / Kölner Phonetik), and the user-facing learning window.

mod ax_reader;
mod differ;
mod session;
mod store;
// `pub(crate)` (not re-exported): the `#[tauri::command]` in `toast` expands
// sibling helper items that `collect_commands!` resolves by module path, which a
// `pub use` of the function alone would not carry — so callers use the full
// `correction_learning::toast::*` path.
pub(crate) mod toast;

pub use differ::Aggressiveness;
pub use session::{begin_session, LearnedCorrectionEvent, LearnedCorrectionsChanged};
pub use store::{remove, upsert, CorrectionSource, LearnedCorrection};

use crate::settings::AppSettings;

/// Apply enabled learned corrections to `text`.
///
/// Matching is word-boundary exact and case-insensitive over Unicode word
/// tokens, and the longest phrase wins — so a manually added `New York → NYC` is
/// preferred over a `York → Yorkshire`, and `cat → dog` never rewrites the
/// middle of `category`. (Multi-word and abbreviation pairs like `New York →
/// NYC` are an apply-stage capability driven by manual adds; the Conservative
/// auto-learning gate rejects word-count-changing pairs, so they are not learned
/// on their own.) A language-tagged pair is applied only when the current
/// transcription resolves to that same language; untagged pairs (all manual
/// adds) always apply. Returns `text` unchanged when the feature is disabled or
/// no pair matches.
pub fn apply_learned(text: &str, settings: &AppSettings) -> String {
    if !settings.learn_corrections_enabled {
        return text.to_string();
    }
    apply_corrections(
        text,
        &settings.learned_corrections,
        &resolved_language(settings),
    )
}

/// The language code that drives both learning and applying: the transcription
/// language decides how a word is heard, so `selected_language` wins; its default
/// `"auto"` names no language, so the UI language (`app_language`) fills in. The
/// session tags auto-learned pairs with this code and the apply stage gates on
/// it, so both sides resolve it identically.
pub(crate) fn resolved_language(settings: &AppSettings) -> String {
    if settings.selected_language == "auto" {
        settings.app_language.clone()
    } else {
        settings.selected_language.clone()
    }
}

/// A learned pair compiled for matching: `misheard` split into lowercased word
/// tokens (the longest-first ordering key) plus the verbatim `intended` text.
struct CompiledPair {
    words: Vec<String>,
    intended: String,
}

/// The substitution core, split from [`apply_learned`] so it is testable without
/// constructing a full [`AppSettings`]. `current_lang` is the resolved language
/// of the text being corrected; language-tagged pairs whose tag differs are
/// skipped.
fn apply_corrections(text: &str, corrections: &[LearnedCorrection], current_lang: &str) -> String {
    let pairs = compile_pairs(corrections, current_lang);
    if pairs.is_empty() {
        return text.to_string();
    }

    let tokens = lex(text);
    let mut out = String::new();
    let mut i = 0;
    while i < tokens.len() {
        if let Token::Word(_) = &tokens[i] {
            if let Some((pair, end)) = pairs
                .iter()
                .find_map(|pair| match_pair(&tokens, i, pair).map(|end| (pair, end)))
            {
                out.push_str(&pair.intended);
                i = end;
                continue;
            }
        }
        out.push_str(tokens[i].text());
        i += 1;
    }
    out
}

/// Compile the enabled corrections into a longest-first match table, dropping
/// language-tagged pairs that do not match `current_lang`.
fn compile_pairs(corrections: &[LearnedCorrection], current_lang: &str) -> Vec<CompiledPair> {
    let mut pairs: Vec<CompiledPair> = corrections
        .iter()
        .filter(|correction| correction.enabled)
        .filter(|correction| match &correction.lang {
            Some(lang) => lang == current_lang,
            None => true,
        })
        .filter_map(|correction| {
            let words: Vec<String> = correction
                .misheard
                .split_whitespace()
                .map(str::to_lowercase)
                .collect();
            if words.is_empty() {
                return None;
            }
            Some(CompiledPair {
                words,
                intended: correction.intended.clone(),
            })
        })
        .collect();

    // Longest phrase first: more trigger words, then more characters.
    pairs.sort_by(|a, b| {
        b.words
            .len()
            .cmp(&a.words.len())
            .then_with(|| trigger_len(b).cmp(&trigger_len(a)))
    });

    pairs
}

fn trigger_len(pair: &CompiledPair) -> usize {
    pair.words.iter().map(|word| word.chars().count()).sum()
}

/// Try to match `pair.words` starting exactly at word token `start`. Trigger
/// words must be separated by whitespace only. Returns the token index just past
/// the matched phrase.
fn match_pair(tokens: &[Token], start: usize, pair: &CompiledPair) -> Option<usize> {
    let mut ti = start;
    for (idx, word) in pair.words.iter().enumerate() {
        match tokens.get(ti) {
            Some(Token::Word(w)) if w.to_lowercase() == *word => {}
            _ => return None,
        }
        ti += 1;

        // Between two trigger words there must be only whitespace.
        if idx + 1 < pair.words.len() {
            let mut saw_space = false;
            while let Some(Token::Space(_)) = tokens.get(ti) {
                saw_space = true;
                ti += 1;
            }
            if !saw_space {
                return None;
            }
        }
    }
    Some(ti)
}

/// A Unicode-aware lexical token. Kept local to this module (rather than shared
/// with `text_rules`) so the feature stays independently droppable.
enum Token {
    /// A maximal run of alphanumeric (Unicode-aware) characters.
    Word(String),
    /// A maximal run of whitespace.
    Space(String),
    /// A maximal run of any other characters (existing punctuation/symbols).
    Other(String),
}

impl Token {
    fn text(&self) -> &str {
        match self {
            Token::Word(t) | Token::Space(t) | Token::Other(t) => t,
        }
    }
}

/// Split `text` into [`Token`]s, preserving the original characters exactly.
/// Splitting on Unicode character classes keeps umlauts and `ß` inside their
/// words, so a learned `Müller` never matches inside `Müllerstraße`.
fn lex(text: &str) -> Vec<Token> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Word,
        Space,
        Other,
    }

    fn classify(c: char) -> Class {
        if c.is_alphanumeric() {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    }

    fn make_token(class: Class, text: String) -> Token {
        match class {
            Class::Word => Token::Word(text),
            Class::Space => Token::Space(text),
            Class::Other => Token::Other(text),
        }
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_class: Option<Class> = None;

    for c in text.chars() {
        let class = classify(c);
        match current_class {
            Some(existing) if existing == class => current.push(c),
            _ => {
                if let Some(existing) = current_class {
                    tokens.push(make_token(existing, std::mem::take(&mut current)));
                }
                current.push(c);
                current_class = Some(class);
            }
        }
    }

    if let Some(existing) = current_class {
        tokens.push(make_token(existing, current));
    }

    tokens
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(misheard: &str, intended: &str) -> LearnedCorrection {
        LearnedCorrection::new(misheard, intended, CorrectionSource::Manual, 0)
    }

    fn disabled(misheard: &str, intended: &str) -> LearnedCorrection {
        let mut correction = pair(misheard, intended);
        correction.enabled = false;
        correction
    }

    #[test]
    fn replaces_at_word_boundary() {
        let corrections = vec![pair("cat", "dog")];
        assert_eq!(
            apply_corrections("the cat sat", &corrections, "en"),
            "the dog sat"
        );
    }

    #[test]
    fn does_not_corrupt_substrings() {
        // `cat` must not rewrite the middle of `category`.
        let corrections = vec![pair("cat", "dog")];
        assert_eq!(
            apply_corrections("a category list", &corrections, "en"),
            "a category list"
        );
    }

    #[test]
    fn longest_phrase_wins() {
        let corrections = vec![pair("York", "Yorkshire"), pair("New York", "NYC")];
        assert_eq!(
            apply_corrections("New York rocks", &corrections, "en"),
            "NYC rocks"
        );
    }

    #[test]
    fn case_insensitive_match_keeps_intended_casing() {
        let corrections = vec![pair("munchen", "München")];
        assert_eq!(
            apply_corrections("Ich war in Munchen", &corrections, "en"),
            "Ich war in München"
        );
    }

    #[test]
    fn unicode_boundary_is_respected() {
        // A learned umlaut word must not match inside a longer compound token.
        let corrections = vec![pair("Müller", "Møller")];
        assert_eq!(
            apply_corrections("Herr Müller und Müllerstraße", &corrections, "en"),
            "Herr Møller und Müllerstraße"
        );
    }

    #[test]
    fn disabled_entries_are_skipped() {
        let corrections = vec![disabled("cat", "dog")];
        assert_eq!(
            apply_corrections("the cat sat", &corrections, "en"),
            "the cat sat"
        );
    }

    #[test]
    fn language_tagged_pairs_only_apply_to_that_language() {
        let mut de = pair("Munchen", "München");
        de.lang = Some("de".to_string());
        let corrections = vec![de];
        // Applied when the transcription language matches.
        assert_eq!(
            apply_corrections("Ich war in Munchen", &corrections, "de"),
            "Ich war in München"
        );
        // Skipped for a different language.
        assert_eq!(
            apply_corrections("Ich war in Munchen", &corrections, "en"),
            "Ich war in Munchen"
        );
    }

    #[test]
    fn untagged_pairs_apply_to_any_language() {
        let corrections = vec![pair("cat", "dog")];
        assert_eq!(
            apply_corrections("the cat sat", &corrections, "de"),
            "the dog sat"
        );
    }
}
