//! Auto-learned corrections (fork feature: voice-control).
//!
//! A second, *exact* dictionary that complements Whisper/Parakeet's fuzzy
//! output: pairs of `misheard → intended` text substituted deterministically at
//! transcription time (VoiceInk-style word replacements), learned Wispr-Flow
//! style from the user's own edits:
//!
//! - [`session`] — after a paste, watches the focused field (macOS
//!   Accessibility) and diffs the user's edit against the pasted span.
//! - [`differ`] — isolates the edit and gates it: vocabulary (names, jargon),
//!   not grammar.
//! - [`store`] — suggestions → active after recurrence or confirmation; undo
//!   blocks a pair for good; persisted apart from `AppSettings`.
//! - [`apply_learned`] — the deterministic apply stage in the transcription
//!   funnel; only active, enabled pairs fire.
//! - [`toast`] — suggestions with Accept, promotions with Undo; ⌃⎋ dismisses.
//! - [`toast_shortcuts`] — keyboard shortcuts for those buttons, registered
//!   only while the toast is visible.
//! - [`commands`] — the review UI's command surface.

// `pub(crate)`: the dictation-context capture reuses the AX helpers.
pub(crate) mod ax_reader;
pub(crate) mod commands;
mod differ;
pub(crate) mod lexicon; // `is_name` also gates self-correction cues.
mod session;
mod store;
// `pub(crate)` (not re-exported): the `#[tauri::command]` in `toast` expands
// sibling helper items that `collect_commands!` resolves by module path, which a
// `pub use` of the function alone would not carry — so callers use the full
// `correction_learning::toast::*` path.
pub(crate) mod toast;
// `pub(crate)` for the same `collect_commands!` reason, and for the hook in
// `shortcut::handler`.
pub(crate) mod toast_shortcuts;

pub use differ::Aggressiveness;
pub use session::{begin_session, end_session};
pub use store::{CorrectionStatus, LearnedCorrection};

use crate::audio_toolkit::{detect_output_language, OutputLanguageEvidence};
use crate::settings::AppSettings;
use log::debug;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::{Arc, Mutex, OnceLock};

/// The live settings store lets vocabulary consumers respect the master
/// switch without needing an app handle or hiding pairs from the review UI.
static SETTINGS_STORE: OnceLock<Arc<tauri_plugin_store::Store<tauri::Wry>>> = OnceLock::new();

/// Learned pairs available to transcription consumers. Disabled means empty;
/// the persisted list stays available through the review commands.
pub(crate) fn snapshot() -> Arc<store::LearnedCorrections> {
    let enabled = SETTINGS_STORE
        .get()
        .and_then(|store| store.get("settings"))
        .and_then(|settings| settings.get("learn_corrections_enabled")?.as_bool())
        .unwrap_or(false);
    gated_snapshot(store::snapshot(), enabled)
}

fn gated_snapshot(
    state: Arc<store::LearnedCorrections>,
    enabled: bool,
) -> Arc<store::LearnedCorrections> {
    if enabled {
        state
    } else {
        Arc::default()
    }
}

/// Emitted when a learning session stored or promoted pairs. The toast window
/// renders it. Suggestions and promotions are kept apart so each action only
/// touches the pairs it is about.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LearnedCorrectionEvent {
    /// The first pair of the group, shown verbatim.
    pub id: String,
    pub misheard: String,
    pub intended: String,
    /// The first pair's status after this edit: `suggested` (stored, not yet
    /// applied) or `active` (now applied).
    pub status: CorrectionStatus,
    /// The new suggestions of this group; Accept acts on these only.
    pub suggested_ids: Vec<String>,
    /// The pairs this group promoted to active; Undo rejects these only.
    pub active_ids: Vec<String>,
    /// Pairs beyond the first, rendered as "+N more".
    pub extra: u32,
}

/// Fired on every change of the learned list / block list (auto-learning,
/// promotion, undo, UI edits). The review UI re-fetches
/// `get_learned_corrections` on it.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LearnedCorrectionsChanged {}

/// Load the persisted pairs at startup (also migrates the legacy list out of
/// `AppSettings`), so the first transcription already applies them.
pub fn init(app: &tauri::AppHandle) {
    if let Some(settings_store) = store::store(app) {
        let _ = SETTINGS_STORE.set(settings_store);
    }
    store::init(app);
}

/// The base ISO code of a language tag (`en-DE` → `en`, `zh_Hant` → `zh`).
pub(crate) fn base_language(tag: &str) -> String {
    tag.split(['-', '_'])
        .next()
        .unwrap_or(tag)
        .to_ascii_lowercase()
}

/// The language of the most recent transcription, recorded by the apply stage
/// so the following paste's learning session tags and phonetically gates its
/// pairs with the language that was actually spoken (not the UI locale).
static LAST_LANGUAGE: Mutex<Option<String>> = Mutex::new(None);

pub(crate) fn last_transcription_language() -> Option<String> {
    LAST_LANGUAGE.lock().ok().and_then(|lang| lang.clone())
}

/// Resolve the transcription's language: the model/user evidence first, else a
/// confidence-gated detection on the text itself.
fn resolve_language(
    text: &str,
    evidence: &OutputLanguageEvidence,
    supported_languages: &[String],
) -> Option<String> {
    evidence
        .language()
        .map(str::to_string)
        .or_else(|| detect_output_language(text, supported_languages))
        .map(|lang| base_language(&lang))
}

/// Apply active learned corrections to `text`.
///
/// Matching is token-exact (hyphens/apostrophes must match, words compare
/// case-insensitively), longest phrase first, and never inside URLs, paths or
/// e-mail addresses. A language-tagged pair applies only when the
/// transcription's language matches, or when it could not be determined.
pub fn apply_learned(
    text: &str,
    settings: &AppSettings,
    output_language: &OutputLanguageEvidence,
    supported_languages: &[String],
) -> String {
    let lang = resolve_language(text, output_language, supported_languages);
    if let Ok(mut last) = LAST_LANGUAGE.lock() {
        last.clone_from(&lang);
    }
    if !settings.learn_corrections_enabled {
        return text.to_string();
    }
    let state = store::snapshot();
    let (out, fired) = apply_corrections(text, &state.corrections, lang.as_deref());
    for (id, from, to) in fired {
        debug!(
            "learn-apply: pair {} fired ({} chars -> {} chars)",
            id, from, to
        );
    }
    out
}

// --- tokenization --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Word,
    Space,
    Other,
}

#[derive(Debug, Clone)]
struct Token<'a> {
    class: Class,
    text: &'a str,
}

/// Split into maximal runs of alphanumerics, whitespace and everything else,
/// preserving the text exactly. Misheard spans and transcripts use the same
/// lexer, so `Chat-GPT` (`Chat`,`-`,`GPT`) matches token for token.
fn lex(text: &str) -> Vec<Token<'_>> {
    fn classify(c: char) -> Class {
        if c.is_alphanumeric() {
            Class::Word
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    }
    let mut tokens = Vec::new();
    let mut start = 0;
    let mut current: Option<Class> = None;
    for (idx, c) in text.char_indices() {
        let class = classify(c);
        match current {
            Some(existing) if existing == class => {}
            Some(existing) => {
                tokens.push(Token {
                    class: existing,
                    text: &text[start..idx],
                });
                start = idx;
                current = Some(class);
            }
            None => current = Some(class),
        }
    }
    if let Some(existing) = current {
        tokens.push(Token {
            class: existing,
            text: &text[start..],
        });
    }
    tokens
}

/// Per token: whether it belongs to a whitespace-delimited chunk that looks
/// like a URL, path or e-mail address, where substitutions must never land.
fn protected_tokens(tokens: &[Token]) -> Vec<bool> {
    let mut protected = vec![false; tokens.len()];
    let mut idx = 0;
    while idx < tokens.len() {
        if tokens[idx].class == Class::Space {
            idx += 1;
            continue;
        }
        let start = idx;
        while idx < tokens.len() && tokens[idx].class != Class::Space {
            idx += 1;
        }
        let chunk: String = tokens[start..idx].iter().map(|t| t.text).collect();
        if is_protected_chunk(&chunk) {
            protected[start..idx].iter_mut().for_each(|p| *p = true);
        }
    }
    protected
}

fn is_protected_chunk(chunk: &str) -> bool {
    let lower = chunk.to_lowercase();
    if lower.contains("://") || lower.starts_with("www.") {
        return true;
    }
    if let Some(at) = chunk.find('@') {
        if chunk[at + 1..].contains('.') && at > 0 {
            return true;
        }
    }
    // Paths: a separator between two non-separator characters (`src/main.rs`,
    // `~/Code`, `C:\Users`).
    let chars: Vec<char> = chunk.chars().collect();
    let is_sep = |c: char| c == '/' || c == '\\';
    if chunk.starts_with("~/")
        || chunk.starts_with('/')
        || chars
            .windows(3)
            .any(|w| is_sep(w[1]) && !is_sep(w[0]) && !is_sep(w[2]))
    {
        return true;
    }
    // Domains / file names: a dot between two alphanumerics (`handy.computer`,
    // `main.rs`).
    chars
        .windows(3)
        .any(|w| w[1] == '.' && w[0].is_alphanumeric() && w[2].is_alphanumeric())
}

// --- apply ---------------------------------------------------------------------

/// A pattern token of a compiled pair.
#[derive(Debug)]
enum Pattern {
    /// Lowercased word, compared case-insensitively.
    Word(String),
    /// Any whitespace run.
    Space,
    /// Exact punctuation.
    Other(String),
}

struct CompiledPair<'a> {
    pattern: Vec<Pattern>,
    entry: &'a LearnedCorrection,
}

fn compile_pairs<'a>(
    corrections: &'a [LearnedCorrection],
    lang: Option<&str>,
) -> Vec<CompiledPair<'a>> {
    let mut pairs: Vec<CompiledPair> = corrections
        .iter()
        .filter(|c| c.is_applied())
        .filter(|c| match (&c.lang, lang) {
            (Some(pair_lang), Some(lang)) => base_language(pair_lang) == lang,
            _ => true,
        })
        .filter_map(|entry| {
            let pattern: Vec<Pattern> = lex(entry.misheard.trim())
                .into_iter()
                .map(|t| match t.class {
                    Class::Word => Pattern::Word(t.text.to_lowercase()),
                    Class::Space => Pattern::Space,
                    Class::Other => Pattern::Other(t.text.to_string()),
                })
                .collect();
            pattern
                .iter()
                .any(|p| matches!(p, Pattern::Word(_)))
                .then_some(CompiledPair { pattern, entry })
        })
        .collect();
    // Longest phrase first: more words, then more characters.
    let words = |p: &CompiledPair| {
        p.pattern
            .iter()
            .filter(|t| matches!(t, Pattern::Word(_)))
            .count()
    };
    pairs.sort_by(|a, b| {
        words(b)
            .cmp(&words(a))
            .then_with(|| b.entry.misheard.len().cmp(&a.entry.misheard.len()))
    });
    pairs
}

/// Match `pattern` at token `start`; returns the index just past the match.
fn match_at(
    tokens: &[Token],
    lowercase: &[String],
    protected: &[bool],
    start: usize,
    pattern: &[Pattern],
) -> Option<usize> {
    let mut ti = start;
    for p in pattern {
        let token = tokens.get(ti)?;
        if protected[ti] {
            return None;
        }
        let ok = match (p, token.class) {
            (Pattern::Word(w), Class::Word) => lowercase[ti] == *w,
            (Pattern::Space, Class::Space) => true,
            (Pattern::Other(o), Class::Other) => token.text == o,
            _ => false,
        };
        if !ok {
            return None;
        }
        ti += 1;
    }
    Some(ti)
}

/// The intended text with its casing adapted to where it lands.
///
/// - Mixed case (`iPhone`, `ChatGPT`), digits+letters (`k8s`) and all-caps
///   stay verbatim.
/// - A lowercase intended word is capitalised at a sentence start.
/// - A capitalised intended word learned at a sentence start (its capital
///   may be positional) follows the transcript's casing mid-sentence;
///   otherwise (proper noun, German noun) it stays capitalised.
fn adapt_case(entry: &LearnedCorrection, matched_first: &str, sentence_start: bool) -> String {
    let intended = entry.intended.as_str();
    let letters: Vec<char> = intended.chars().filter(|c| c.is_alphanumeric()).collect();
    let mixed = letters.iter().skip(1).any(|c| c.is_uppercase())
        || (letters.iter().any(|c| c.is_ascii_digit())
            && letters.iter().any(|c| c.is_alphabetic()));
    if mixed || letters.is_empty() {
        return intended.to_string();
    }
    let mut chars = intended.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let rest = chars.as_str();
    if first.is_lowercase() {
        if sentence_start {
            return first.to_uppercase().chain(rest.chars()).collect();
        }
        return intended.to_string();
    }
    if first.is_uppercase() && entry.sentence_start && !sentence_start {
        let transcript_lower = matched_first.chars().next().is_some_and(char::is_lowercase);
        if transcript_lower {
            return first.to_lowercase().chain(rest.chars()).collect();
        }
    }
    intended.to_string()
}

/// The substitution core, split from [`apply_learned`] so it is testable
/// without settings or a store. Returns the text plus `(id, from, to)`
/// lengths for every fired pair (for content-free logging).
fn apply_corrections(
    text: &str,
    corrections: &[LearnedCorrection],
    lang: Option<&str>,
) -> (String, Vec<(String, usize, usize)>) {
    let pairs = compile_pairs(corrections, lang);
    if pairs.is_empty() {
        return (text.to_string(), Vec::new());
    }
    let tokens = lex(text);
    let lowercase: Vec<String> = tokens
        .iter()
        .map(|token| {
            if token.class == Class::Word {
                token.text.to_lowercase()
            } else {
                String::new()
            }
        })
        .collect();
    let protected = protected_tokens(&tokens);
    let mut out = String::with_capacity(text.len());
    let mut fired = Vec::new();
    let mut i = 0;
    let mut offset = 0;
    while i < tokens.len() {
        // A match may only start where a token starts a fresh word/punct run
        // (tokens are maximal, so any index is a boundary).
        let hit = (tokens[i].class != Class::Space)
            .then(|| {
                pairs.iter().find_map(|p| {
                    match_at(&tokens, &lowercase, &protected, i, &p.pattern).map(|end| (p, end))
                })
            })
            .flatten();
        if let Some((pair, end)) = hit {
            let matched: String = tokens[i..end].iter().map(|t| t.text).collect();
            let replacement = adapt_case(
                pair.entry,
                tokens[i].text,
                differ::is_sentence_start(text, offset),
            );
            fired.push((
                pair.entry.id.clone(),
                matched.chars().count(),
                replacement.chars().count(),
            ));
            out.push_str(&replacement);
            offset += matched.len();
            i = end;
            continue;
        }
        out.push_str(tokens[i].text);
        offset += tokens[i].text.len();
        i += 1;
    }
    (out, fired)
}

#[cfg(test)]
mod tests {
    use super::store::CorrectionSource;
    use super::*;

    fn pair(misheard: &str, intended: &str) -> LearnedCorrection {
        LearnedCorrection::new(misheard, intended, CorrectionSource::Manual, 0)
    }

    #[test]
    fn master_switch_hides_pairs_without_changing_store() {
        let state = Arc::new(store::LearnedCorrections {
            corrections: vec![pair("Klaster", "Cluster")],
            ..Default::default()
        });
        let disabled = gated_snapshot(state.clone(), false);
        assert!(disabled.corrections.is_empty());
        assert!(disabled.applied_pairs().is_empty());
        let enabled = gated_snapshot(state.clone(), true);
        assert!(Arc::ptr_eq(&enabled, &state));
        assert_eq!(enabled.corrections.len(), 1);
        assert_eq!(enabled.applied_pairs().len(), 1);
    }

    #[test]
    fn disabled_dictionary_still_records_transcription_language() {
        let settings = crate::settings::get_default_settings();
        assert!(!settings.learn_corrections_enabled);
        let text = "Hallo Cluster";
        assert_eq!(
            apply_learned(
                text,
                &settings,
                &OutputLanguageEvidence::ModelDetected("de-DE".into()),
                &[]
            ),
            text
        );
        assert_eq!(last_transcription_language().as_deref(), Some("de"));
        apply_learned("x", &settings, &OutputLanguageEvidence::Unknown, &[]);
        assert_eq!(last_transcription_language(), None);
    }

    #[test]
    fn closing_quotes_preserve_sentence_start_casing() {
        let mut positional = pair("Klaster", "Cluster");
        positional.sentence_start = true;
        let lowercase = pair("Klaster", "cluster");
        for prefix in ["\"Hallo.\" ", "„Hallo!“ ", "»Hallo?« "] {
            let text = format!("{prefix}klaster");
            let expected = format!("{prefix}Cluster");
            assert_eq!(apply(&text, std::slice::from_ref(&positional)), expected);
            assert_eq!(apply(&text, std::slice::from_ref(&lowercase)), expected);
        }
        assert_eq!(
            apply("\"Hallo\" klaster", &[positional]),
            "\"Hallo\" cluster"
        );
    }

    fn apply(text: &str, corrections: &[LearnedCorrection]) -> String {
        apply_corrections(text, corrections, Some("en")).0
    }

    #[test]
    fn replaces_at_word_boundary_only() {
        let c = vec![pair("cat", "dog")];
        assert_eq!(apply("the cat sat", &c), "the dog sat");
        assert_eq!(apply("a category list", &c), "a category list");
    }

    #[test]
    fn longest_phrase_wins() {
        let c = vec![pair("York", "Yorkshire"), pair("New York", "NYC")];
        assert_eq!(apply("in New York now", &c), "in NYC now");
    }

    #[test]
    fn unicode_boundary_is_respected() {
        let c = vec![pair("Müller", "Møller")];
        assert_eq!(
            apply("Herr Müller und Müllerstraße", &c),
            "Herr Møller und Müllerstraße"
        );
    }

    #[test]
    fn only_active_enabled_pairs_apply() {
        let mut suggested = pair("cat", "dog");
        suggested.status = CorrectionStatus::Suggested;
        assert_eq!(apply("the cat sat", &[suggested]), "the cat sat");
        let mut disabled = pair("cat", "dog");
        disabled.enabled = false;
        assert_eq!(apply("the cat sat", &[disabled]), "the cat sat");
    }

    #[test]
    fn hyphenated_misheard_matches_token_for_token() {
        let c = vec![pair("Chat-GTP", "ChatGPT"), pair("e-mal", "e-mail")];
        assert_eq!(
            apply("ask Chat-GTP for an e-mal draft", &c),
            "ask ChatGPT for an e-mail draft"
        );
        // A bare `Chat GTP` is a different token sequence.
        assert_eq!(apply("ask Chat GTP", &c), "ask Chat GTP");
    }

    #[test]
    fn urls_paths_and_emails_are_skipped() {
        let c = vec![pair("handy", "Handy")];
        assert_eq!(
            apply(
                "see https://handy.computer/handy and ~/code/handy or me@handy.dev, handy rocks",
                &c
            ),
            "see https://handy.computer/handy and ~/code/handy or me@handy.dev, Handy rocks"
        );
        assert_eq!(apply("open handy.computer", &c), "open handy.computer");
    }

    #[test]
    fn casing_adapts_to_position() {
        // Learned at a sentence start: the capital is positional.
        let mut positional = pair("Klaster", "Cluster");
        positional.sentence_start = true;
        assert_eq!(
            apply("we run a klaster here", std::slice::from_ref(&positional)),
            "we run a cluster here"
        );
        assert_eq!(
            apply("Klaster is up.", std::slice::from_ref(&positional)),
            "Cluster is up."
        );
        // A proper noun learned mid-sentence keeps its capital.
        let name = pair("jon", "John");
        assert_eq!(
            apply("ask jon now", std::slice::from_ref(&name)),
            "ask John now"
        );
        // A lowercase intended word is capitalised at a sentence start.
        let lower = pair("Cubectl", "kubectl");
        assert_eq!(
            apply("Hi. Cubectl apply", std::slice::from_ref(&lower)),
            "Hi. Kubectl apply"
        );
        // Mixed case and jargon stay verbatim everywhere.
        let mixed = pair("kates", "k8s");
        assert_eq!(
            apply("Kates rocks", std::slice::from_ref(&mixed)),
            "k8s rocks"
        );
        let iphone = pair("I phone", "iPhone");
        assert_eq!(
            apply("I phone is new", std::slice::from_ref(&iphone)),
            "iPhone is new"
        );
    }

    #[test]
    fn language_tagged_pairs_respect_the_transcription_language() {
        let mut de = pair("Munchen", "München");
        de.lang = Some("de".into());
        let c = vec![de];
        assert_eq!(
            apply_corrections("in Munchen", &c, Some("de")).0,
            "in München"
        );
        assert_eq!(
            apply_corrections("in Munchen", &c, Some("en")).0,
            "in Munchen"
        );
        // Unknown language: the pair itself is the evidence.
        assert_eq!(apply_corrections("in Munchen", &c, None).0, "in München");
    }

    #[test]
    fn language_resolution_prefers_evidence_and_reduces_to_base() {
        let supported: Vec<String> = vec!["de".into(), "en".into()];
        assert_eq!(
            resolve_language(
                "egal",
                &OutputLanguageEvidence::ModelDetected("de".into()),
                &supported
            )
            .as_deref(),
            Some("de")
        );
        assert_eq!(
            resolve_language(
                "x",
                &OutputLanguageEvidence::UserSelected("en-US".into()),
                &supported
            )
            .as_deref(),
            Some("en")
        );
        // Unknown evidence falls back to text detection.
        assert_eq!(
            resolve_language(
                "Ich habe heute einen wichtigen Termin mit dem Kunden in der Stadt und freue mich sehr darauf",
                &OutputLanguageEvidence::Unknown,
                &supported
            )
            .as_deref(),
            Some("de")
        );
        assert_eq!(base_language("en-DE"), "en");
    }

    #[test]
    fn fired_pairs_are_reported_by_id_and_length() {
        let c = vec![pair("cat", "dog")];
        let (_, fired) = apply_corrections("cat and cat", &c, None);
        assert_eq!(fired.len(), 2);
        assert_eq!(fired[0].0, c[0].id);
        assert_eq!((fired[0].1, fired[0].2), (3, 3));
    }
}
