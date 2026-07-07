//! Deterministic text-rules layer (fork feature: voice-control).
//!
//! This module implements a zero-latency, fully deterministic transform stage
//! for transcription output. It converts spoken punctuation/formatting commands
//! (German *and* English) into their symbols and, optionally, applies inverse
//! text normalization (spoken number words → digits).
//!
//! It runs upstream of the optional LLM post-process step, replacing the LLM
//! round-trip for the class of fixed, predictable substitutions.
//!
//! The engine is split into two submodules:
//! - [`substitutions`] — the spoken-command → symbol substitution engine.
//! - [`itn`] — inverse text normalization (number words → digits).

mod itn;
mod substitutions;

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::settings::AppSettings;

/// How a substitution's replacement text is joined to the surrounding words.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpacingPolicy {
    /// No space before, a single space after. Used for trailing punctuation
    /// such as `,` `.` `:` `;` `?` `!`.
    AttachLeft,
    /// A single space before, no space after. Used for characters that open a
    /// span, e.g. `(` in `test (mir geht's gut)`.
    AttachRight,
    /// No spaces on either side. Used for characters that glue two words
    /// together, e.g. `-` `/` `_` in `voice Bindestrich control` → `voice-control`.
    /// Gluing is the safe default for a user-authored symbol replacement.
    #[default]
    Glue,
    /// Replacement inserted as-is with the surrounding spaces collapsed. Used
    /// for block separators such as newline / paragraph and standalone symbols.
    Standalone,
}

/// A single substitution rule: a spoken `trigger` (a word or a multi-word
/// phrase) is replaced by `replacement`, joined according to `spacing`.
///
/// Matching is case-insensitive and Unicode-aware; see [`substitutions`].
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct TextRule {
    /// The spoken trigger, e.g. `"Komma"` or `"Klammer auf"`.
    pub trigger: String,
    /// The literal text inserted in place of the trigger, e.g. `","`.
    pub replacement: String,
    /// How `replacement` is spaced relative to its neighbours.
    #[serde(default)]
    pub spacing: SpacingPolicy,
}

/// The built-in default substitution table (German + English), shipped in code.
///
/// User rules are merged over these (same trigger → user rule wins) and any
/// entry can be disabled individually via
/// [`AppSettings::text_rules_disabled_builtins`].
pub fn builtin_rules() -> Vec<TextRule> {
    // (trigger, replacement, spacing)
    let table: &[(&str, &str, SpacingPolicy)] = &[
        // Glued symbols (paths, identifiers, domains).
        ("Bindestrich", "-", SpacingPolicy::Glue),
        ("dash", "-", SpacingPolicy::Glue),
        ("Unterstrich", "_", SpacingPolicy::Glue),
        ("underscore", "_", SpacingPolicy::Glue),
        ("Schrägstrich", "/", SpacingPolicy::Glue),
        ("slash", "/", SpacingPolicy::Glue),
        // Ambiguous in normal German speech ("Punkt"); users can disable it via
        // the disabled-builtins list. Kept glued for paths/domains.
        ("Punkt", ".", SpacingPolicy::Glue),
        ("dot", ".", SpacingPolicy::Glue),
        // Trailing punctuation.
        ("Doppelpunkt", ":", SpacingPolicy::AttachLeft),
        ("colon", ":", SpacingPolicy::AttachLeft),
        ("Semikolon", ";", SpacingPolicy::AttachLeft),
        ("semicolon", ";", SpacingPolicy::AttachLeft),
        ("Komma", ",", SpacingPolicy::AttachLeft),
        ("comma", ",", SpacingPolicy::AttachLeft),
        ("Fragezeichen", "?", SpacingPolicy::AttachLeft),
        ("question mark", "?", SpacingPolicy::AttachLeft),
        ("Ausrufezeichen", "!", SpacingPolicy::AttachLeft),
        ("exclamation mark", "!", SpacingPolicy::AttachLeft),
        // Brackets. The opening bracket hugs the word to its right; the closing
        // bracket glues to the word on its left and lets following punctuation
        // (e.g. a list comma) stand.
        ("Klammer auf", "(", SpacingPolicy::AttachRight),
        ("open paren", "(", SpacingPolicy::AttachRight),
        ("Klammer zu", ")", SpacingPolicy::Glue),
        ("close paren", ")", SpacingPolicy::Glue),
        // Standalone symbols and block separators.
        ("Anführungszeichen", "\"", SpacingPolicy::Standalone),
        ("neue Zeile", "\n", SpacingPolicy::Standalone),
        ("new line", "\n", SpacingPolicy::Standalone),
        ("neuer Absatz", "\n\n", SpacingPolicy::Standalone),
        ("new paragraph", "\n\n", SpacingPolicy::Standalone),
    ];

    table
        .iter()
        .map(|(trigger, replacement, spacing)| TextRule {
            trigger: (*trigger).to_string(),
            replacement: (*replacement).to_string(),
            spacing: *spacing,
        })
        .collect()
}

/// Apply the deterministic text-rules layer to `text`.
///
/// Returns `text` unchanged when the feature is disabled. When enabled, inverse
/// text normalization (if enabled) runs first so that spoken decimal markers
/// (`Komma` / `point`) are still present as words, then the substitution engine
/// converts the remaining spoken commands into symbols.
pub fn apply_text_rules(text: &str, settings: &AppSettings) -> String {
    if !settings.text_rules_enabled {
        return text.to_string();
    }

    let mut out = text.to_string();

    if settings.text_rules_itn_enabled {
        out = itn::apply_itn(&out);
    }

    substitutions::apply_substitutions(
        &out,
        &settings.text_rules_custom,
        &settings.text_rules_disabled_builtins,
    )
}

/// A lexical token used by both submodules. Splitting on Unicode character
/// classes (rather than a regex `\b`) keeps German umlauts and `ß` inside their
/// words, so `Bindestrichen` never matches the trigger `Bindestrich`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Token {
    /// A maximal run of alphanumeric (Unicode-aware) characters.
    Word(String),
    /// A maximal run of whitespace.
    Space(String),
    /// A maximal run of any other characters (existing punctuation/symbols),
    /// preserved verbatim.
    Other(String),
}

impl Token {
    /// The token's verbatim characters, regardless of class.
    pub(crate) fn text(&self) -> &str {
        match self {
            Token::Word(t) | Token::Space(t) | Token::Other(t) => t,
        }
    }
}

/// Split `text` into [`Token`]s, preserving the original characters exactly.
pub(crate) fn lex(text: &str) -> Vec<Token> {
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
