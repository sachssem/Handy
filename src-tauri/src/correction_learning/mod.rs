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
//! - **Phase D** — aggressiveness settings and phonetic gating.

mod differ;
mod store;

pub use store::{remove, upsert, CorrectionSource, LearnedCorrection};

use crate::settings::AppSettings;

/// Apply enabled learned corrections to `text`.
///
/// Matching is word-boundary exact and case-insensitive over Unicode word
/// tokens, and the longest phrase wins — so a learned `New York → NYC` is
/// preferred over a learned `York → Yorkshire`, and `cat → dog` never rewrites
/// the middle of `category`. Returns `text` unchanged when the feature is
/// disabled or no pair matches.
pub fn apply_learned(text: &str, settings: &AppSettings) -> String {
    if !settings.learn_corrections_enabled {
        return text.to_string();
    }
    apply_corrections(text, &settings.learned_corrections)
}

/// A learned pair compiled for matching: `misheard` split into lowercased word
/// tokens (the longest-first ordering key) plus the verbatim `intended` text.
struct CompiledPair {
    words: Vec<String>,
    intended: String,
}

/// The substitution core, split from [`apply_learned`] so it is testable without
/// constructing a full [`AppSettings`].
fn apply_corrections(text: &str, corrections: &[LearnedCorrection]) -> String {
    let pairs = compile_pairs(corrections);
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

/// Compile the enabled corrections into a longest-first match table.
fn compile_pairs(corrections: &[LearnedCorrection]) -> Vec<CompiledPair> {
    let mut pairs: Vec<CompiledPair> = corrections
        .iter()
        .filter(|correction| correction.enabled)
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
            apply_corrections("the cat sat", &corrections),
            "the dog sat"
        );
    }

    #[test]
    fn does_not_corrupt_substrings() {
        // `cat` must not rewrite the middle of `category`.
        let corrections = vec![pair("cat", "dog")];
        assert_eq!(
            apply_corrections("a category list", &corrections),
            "a category list"
        );
    }

    #[test]
    fn longest_phrase_wins() {
        let corrections = vec![pair("York", "Yorkshire"), pair("New York", "NYC")];
        assert_eq!(
            apply_corrections("New York rocks", &corrections),
            "NYC rocks"
        );
    }

    #[test]
    fn case_insensitive_match_keeps_intended_casing() {
        let corrections = vec![pair("munchen", "München")];
        assert_eq!(
            apply_corrections("Ich war in Munchen", &corrections),
            "Ich war in München"
        );
    }

    #[test]
    fn unicode_boundary_is_respected() {
        // A learned umlaut word must not match inside a longer compound token.
        let corrections = vec![pair("Müller", "Møller")];
        assert_eq!(
            apply_corrections("Herr Müller und Müllerstraße", &corrections),
            "Herr Møller und Müllerstraße"
        );
    }

    #[test]
    fn disabled_entries_are_skipped() {
        let corrections = vec![disabled("cat", "dog")];
        assert_eq!(
            apply_corrections("the cat sat", &corrections),
            "the cat sat"
        );
    }
}
