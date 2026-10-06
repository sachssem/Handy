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
//! The engine is split into submodules:
//! - [`substitutions`] — the spoken-command → symbol substitution engine.
//! - [`context`] — command-vs-prose detection for ambiguous command words.
//! - [`lists`] — spoken list markers → numbered / bulleted lines.
//! - [`quotes`] — paired spoken quotation marks.
//! - [`links`] — domains, file names, versions and e-mail addresses.
//! - [`itn`] — inverse text normalization (number words → digits).

mod context;
mod itn;
mod links;
mod lists;
mod quotes;
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

/// How a built-in command decides whether a spoken word is meant as a command.
///
/// Many command words are also ordinary nouns or verbs ("Der Punkt ist …",
/// "Press the dash key"), so built-ins only fire in *command context*. User
/// rules are always unconditional (the user chose the trigger). See
/// [`context`] for the rules each gate applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Fires wherever it matches (multi-word phrases nobody says in prose).
    Always,
    /// Glue symbols (`-` `_` `/` `~` `@`): require a clause/path signal and no
    /// noun/verb use.
    Glue,
    /// Trailing punctuation (`,` `:` `;` `?` `!`): fire unless used as a noun
    /// or there is nothing before it to punctuate, and require a clause signal.
    Punct,
    /// The sentence period (`Punkt` / `dot`): requires a clause boundary;
    /// neighboring path commands alone are insufficient. Domain/file/version
    /// dots are handled earlier by the [`links`] pass.
    Strict,
    /// Not a word substitution: the entry is the on/off key of a dedicated pass
    /// (quote pairing, lists, e-mail `at`). Listed so the UI can disable it.
    Structural,
}

/// Built-in table: (trigger, replacement, spacing, gate).
const BUILTINS: &[(&str, &str, SpacingPolicy, Gate)] = &[
    // Glued symbols (paths, identifiers, domains).
    ("Bindestrich", "-", SpacingPolicy::Glue, Gate::Glue),
    ("dash", "-", SpacingPolicy::Glue, Gate::Glue),
    ("Unterstrich", "_", SpacingPolicy::Glue, Gate::Glue),
    ("underscore", "_", SpacingPolicy::Glue, Gate::Glue),
    ("Schrägstrich", "/", SpacingPolicy::Glue, Gate::Glue),
    ("slash", "/", SpacingPolicy::Glue, Gate::Glue),
    // A home-dir tilde opens a path: space before, glued to what follows.
    ("Tilde", "~", SpacingPolicy::AttachRight, Gate::Glue),
    ("Klammeraffe", "@", SpacingPolicy::Glue, Gate::Glue),
    // The period. Also the joiner in domains/files/versions (links pass).
    ("Punkt", ".", SpacingPolicy::Glue, Gate::Strict),
    ("dot", ".", SpacingPolicy::Glue, Gate::Strict),
    // Trailing punctuation.
    ("Doppelpunkt", ":", SpacingPolicy::AttachLeft, Gate::Punct),
    ("colon", ":", SpacingPolicy::AttachLeft, Gate::Punct),
    ("Semikolon", ";", SpacingPolicy::AttachLeft, Gate::Punct),
    ("semicolon", ";", SpacingPolicy::AttachLeft, Gate::Punct),
    ("Komma", ",", SpacingPolicy::AttachLeft, Gate::Punct),
    ("comma", ",", SpacingPolicy::AttachLeft, Gate::Punct),
    ("Fragezeichen", "?", SpacingPolicy::AttachLeft, Gate::Punct),
    ("question mark", "?", SpacingPolicy::AttachLeft, Gate::Punct),
    (
        "Ausrufezeichen",
        "!",
        SpacingPolicy::AttachLeft,
        Gate::Punct,
    ),
    (
        "exclamation mark",
        "!",
        SpacingPolicy::AttachLeft,
        Gate::Punct,
    ),
    // Brackets. The opening bracket hugs the word to its right; the closing
    // bracket glues to the word on its left and lets following punctuation
    // (e.g. a list comma) stand.
    ("Klammer auf", "(", SpacingPolicy::AttachRight, Gate::Always),
    ("open paren", "(", SpacingPolicy::AttachRight, Gate::Always),
    ("Klammer zu", ")", SpacingPolicy::Glue, Gate::Always),
    ("close paren", ")", SpacingPolicy::Glue, Gate::Always),
    // Block separators.
    ("neue Zeile", "\n", SpacingPolicy::Standalone, Gate::Always),
    ("new line", "\n", SpacingPolicy::Standalone, Gate::Always),
    (
        "neuer Absatz",
        "\n\n",
        SpacingPolicy::Standalone,
        Gate::Always,
    ),
    (
        "new paragraph",
        "\n\n",
        SpacingPolicy::Standalone,
        Gate::Always,
    ),
    // Structural passes (keys). Quotes: paired `"…"` incl. "auf/zu",
    // "in Anführungszeichen X" / "quote … end quote|unquote", "open/close quote".
    (
        quotes::KEY_DE,
        "\"",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        quotes::KEY_EN,
        "\"",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    // Lists: "Punkt eins … Punkt zwei …", "erstens/zweitens …", bullets.
    (
        lists::KEY_NUMBERED_DE,
        "1.",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        lists::KEY_NUMBERED_EN,
        "1.",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        lists::KEY_ORDINAL_DE,
        "1.",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        lists::KEY_ORDINAL_EN,
        "1.",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        lists::KEY_BULLET_DE,
        "-",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    (
        lists::KEY_BULLET_EN,
        "-",
        SpacingPolicy::Standalone,
        Gate::Structural,
    ),
    // E-mail: "marc at example Punkt com" → `marc@example.com` (also "ät").
    (
        links::KEY_EMAIL_AT,
        "@",
        SpacingPolicy::Glue,
        Gate::Structural,
    ),
];

/// The built-in default command table (German + English), shipped in code.
///
/// User rules are merged over these (same trigger → user rule wins) and any
/// entry can be disabled individually via
/// [`AppSettings::text_rules_disabled_builtins`]. Entries with
/// [`Gate::Structural`] are the on/off keys of the quote, list and e-mail
/// passes rather than plain substitutions.
pub fn builtin_rules() -> Vec<TextRule> {
    BUILTINS
        .iter()
        .map(|(trigger, replacement, spacing, _)| TextRule {
            trigger: (*trigger).to_string(),
            replacement: (*replacement).to_string(),
            spacing: *spacing,
        })
        .collect()
}

/// The built-in table with each entry's [`Gate`], for the substitution engine.
pub(crate) fn builtin_table() -> impl Iterator<Item = (TextRule, Gate)> {
    builtin_rules()
        .into_iter()
        .zip(BUILTINS.iter().map(|entry| entry.3))
}

/// Normalize a trigger for comparison: lowercased, single-spaced.
pub(crate) fn normalize_trigger(trigger: &str) -> String {
    trigger
        .split_whitespace()
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the built-in `key` is active: not disabled and not replaced by a
/// user rule with the same trigger (a user rule always wins, unconditionally).
pub(crate) fn builtin_active(key: &str, custom_rules: &[TextRule], disabled: &[String]) -> bool {
    let key = normalize_trigger(key);
    !disabled.iter().any(|d| normalize_trigger(d) == key)
        && !custom_rules
            .iter()
            .any(|rule| normalize_trigger(&rule.trigger) == key)
}

/// Apply the deterministic text-rules layer to `text`. `language` is the
/// utterance's language code if known (ITN keeps English number words in a
/// German utterance).
///
/// Returns `text` unchanged when the feature is disabled. When enabled the
/// passes run in this order:
/// 1. inverse text normalization (if enabled), first so that spoken decimal
///    markers (`Komma` / `point`) are still present as words;
/// 2. [`lists`] — spoken list markers → one item per line;
/// 3. [`quotes`] — paired spoken quotes → `"…"`;
/// 4. [`links`] — domain/file/version dots and e-mail `at` → joined tokens;
/// 5. [`substitutions`] — the remaining spoken commands, context-gated;
/// 6. a lone path/URL/e-mail loses the ASR's sentence period.
pub fn apply_text_rules(text: &str, settings: &AppSettings, language: Option<&str>) -> String {
    if !settings.text_rules_enabled {
        return text.to_string();
    }
    apply_rules(
        text,
        settings.text_rules_itn_enabled,
        language,
        &settings.text_rules_custom,
        &settings.text_rules_disabled_builtins,
    )
}

/// [`apply_text_rules`] without the settings struct (testable).
fn apply_rules(
    text: &str,
    itn: bool,
    language: Option<&str>,
    custom: &[TextRule],
    disabled: &[String],
) -> String {
    let mut out = text.to_string();

    if itn {
        out = itn::apply_itn(&out, language);
    }

    out = lists::apply_lists(&out, custom, disabled);
    out = quotes::apply_quotes(&out, custom, disabled);
    out = links::apply_links(&out, language, custom, disabled);
    out = substitutions::apply_substitutions(&out, custom, disabled);
    links::strip_lone_token_period(&out)
}

/// Whether the utterance language code is German (`de`, `de-DE`, `de_AT` …).
pub(crate) fn is_german(language: Option<&str>) -> bool {
    language.is_some_and(|lang| {
        lang.split(['-', '_'])
            .next()
            .is_some_and(|base| base.eq_ignore_ascii_case("de"))
    })
}

/// A lexical token shared by the text-rule passes. Splitting on Unicode
/// character classes (rather than a regex `\b`) keeps German umlauts and `ß`
/// inside their words, so `Bindestrichen` never matches the trigger `Bindestrich`.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> String {
        apply_rules(text, false, None, &[], &[])
    }

    /// Prose that uses command words as ordinary words must survive unchanged.
    #[test]
    fn prose_false_positives_stay_prose() {
        for text in [
            "Der Punkt ist, dass wir noch warten müssen.",
            "Das ist ein Komma zu viel.",
            "Das ist der springende Punkt.",
            "Das ist ein wirklich guter Punkt.",
            "Er sagte Anführungszeichen sind wichtig.",
            "Put a comma after the dot.",
            "Press the dash key.",
            "We need to slash costs.",
            "Wir treffen uns um Punkt acht.",
            "Ich setze einen Bindestrich.",
            "Das ist ein großes Fragezeichen.",
            "Look at the dot com bubble.",
            "Erstens habe ich keine Zeit, zweitens habe ich keine Lust.",
            "We met at example.com today.",
        ] {
            assert_eq!(rules(text), text, "input: {text}");
        }
    }

    /// Reconstructed raw Parakeet output for the user's samples.
    #[test]
    fn user_samples() {
        let cases = [
            (
                "Einkaufsliste: Punkt 1 Milch. Punkt 2 Eier und. Punkt drei Brot.",
                "Einkaufsliste:\n1. Milch\n2. Eier und\n3. Brot",
            ),
            (
                "Einkaufsliste Doppelpunkt Punkt eins Milch Punkt zwei Eier Punkt drei Brot",
                "Einkaufsliste:\n1. Milch\n2. Eier\n3. Brot",
            ),
            (
                "Erstens Milch, zweitens Eier, drittens Brot.",
                "1. Milch\n2. Eier\n3. Brot",
            ),
            (
                "Neue Liste Milch, nächster Punkt Eier, nächster Punkt Brot.",
                "- Milch\n- Eier\n- Brot",
            ),
            (
                "Er sagte Anführungszeichen auf Hallo Welt Anführungszeichen zu",
                "Er sagte \"Hallo Welt\"",
            ),
            (
                "Wir nennen das in Anführungszeichen Feature Freeze.",
                "Wir nennen das \"Feature Freeze\".",
            ),
            (
                "He said quote this is fine end quote.",
                "He said \"this is fine\".",
            ),
            ("Tilde slash code slash handy.", "~/code/handy"),
            ("Tilde Slash Code Slash Handy", "~/Code/Handy"),
            (
                "Schreib an marc at example Punkt com",
                "Schreib an marc@example.com",
            ),
            (
                "Gehe auf Handy, Punkt, Computer.",
                "Gehe auf handy.computer.",
            ),
            (
                "Öffne die Datei readme. Punkt, md.",
                "Öffne die Datei readme.md.",
            ),
            (
                "Öffne die Datei readme Punkt md im Ordner docs",
                "Öffne die Datei readme.md im Ordner docs",
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(rules(input), expected, "input: {input}");
        }
    }

    /// Spoken commands in command context keep working.
    #[test]
    fn commands_in_context() {
        let cases = [
            ("Guten Tag, Punkt. Hallo", "Guten Tag. Hallo"),
            ("Guten Tag Punkt", "Guten Tag."),
            (
                "Wir müssen noch warten müssen Punkt",
                "Wir müssen noch warten müssen.",
            ),
            (
                "Hallo Welt Punkt neue Zeile wie geht es",
                "Hallo Welt.\nWie geht es",
            ),
            ("Hallo, Punkt Wie geht es dir?", "Hallo. Wie geht es dir?"),
            ("hallo Komma welt", "hallo, welt"),
            ("Hello comma how are you", "Hello, how are you"),
            ("voice Bindestrich control", "voice-control"),
            ("hallo, Komma welt", "hallo, welt"),
            ("Hello, comma how are you", "Hello, how are you"),
            ("voice, Bindestrich control", "voice-control"),
            ("Bindestrich v", "-v"),
            ("cd Tilde Schrägstrich Code", "cd ~/Code"),
            (
                "schreib an marc Klammeraffe example",
                "schreib an marc@example",
            ),
            ("Version 1 Punkt 2 Punkt 3", "Version 1.2.3"),
        ];
        for (input, expected) in cases {
            assert_eq!(rules(input), expected, "input: {input}");
        }
    }

    #[test]
    fn structural_keys_are_listed_but_not_substituted() {
        let triggers: Vec<String> = builtin_rules().into_iter().map(|r| r.trigger).collect();
        for key in [quotes::KEY_DE, lists::KEY_NUMBERED_DE, links::KEY_EMAIL_AT] {
            assert!(triggers.iter().any(|t| t == key));
        }
        // "at" is only an e-mail joiner, never a plain substitution.
        assert_eq!(rules("I am at home"), "I am at home");
    }

    #[test]
    fn custom_rule_replaces_gating() {
        // A user rule for "Punkt" is unconditional (and disables the links pass
        // for it): the user chose the trigger deliberately.
        let custom = vec![TextRule {
            trigger: "Punkt".to_string(),
            replacement: ".".to_string(),
            spacing: SpacingPolicy::Glue,
        }];
        assert_eq!(
            apply_rules("Der Punkt ist", false, None, &custom, &[]),
            "Der.ist"
        );
    }

    /// Journal samples: English joiners and ticket ids in German dictation.
    #[test]
    fn english_joiners_and_ticket_ids_in_full_pipeline() {
        let de = |text: &str| apply_rules(text, true, Some("de"), &[], &[]);
        let en = |text: &str| apply_rules(text, true, Some("en"), &[], &[]);
        assert_eq!(
            de("Discount dot value wird nur angezeigt, wenn es passt."),
            "Discount.value wird nur angezeigt, wenn es passt."
        );
        assert_eq!(
            de("Was glaube ich auch der PP Dash 106 Branch einführt, oder?"),
            "Was glaube ich auch der PP-106 Branch einführt, oder?"
        );
        assert_eq!(en("Merge PP Dash 106 today."), "Merge PP-106 today.");
        for text in [
            "I made a mad dash for the door.",
            "She wore a polka dot dress.",
        ] {
            assert_eq!(en(text), text, "input: {text}");
        }
    }

    #[test]
    fn email_at_signal_survives_full_pipeline() {
        for text in [
            "Sign up at example.com.",
            "Read more at handy.computer",
            "John at example.com",
        ] {
            assert_eq!(rules(text), text, "input: {text}");
        }
        assert_eq!(
            rules("Find us online at example dot com"),
            "Find us online at example.com"
        );
        assert_eq!(
            rules("Schreib an marc at example Punkt com"),
            "Schreib an marc@example.com"
        );
    }

    #[test]
    fn ordinary_punctuation_and_glue_words_survive_full_pipeline() {
        for text in [
            "He has colon cancer.",
            "There's a big question mark over the budget.",
            "Click the red dot.",
            "Retailers slash prices.",
            "Das schreibt man mit Bindestrich.",
            "I made a mad dash for the door.",
        ] {
            assert_eq!(rules(text), text, "input: {text}");
        }
        for (text, expected) in [
            ("Hallo Komma wie geht's Fragezeichen", "Hallo, wie geht's?"),
            ("Slash users slash marc", "/users/marc"),
            ("readme Punkt md", "readme.md"),
            ("cd Tilde Schrägstrich Code", "cd ~/Code"),
        ] {
            assert_eq!(rules(text), expected, "input: {text}");
        }
    }

    #[test]
    fn rejected_lists_do_not_fall_through_to_punctuation_commands() {
        for text in [
            "erstens bin ich müde, zweitens hungrig",
            "Erstens zu teuer, zweitens zu spät.",
            "Punkt eins ist erledigt, Punkt zwei ist offen.",
            "Item 1 is done, item 2 is open.",
        ] {
            assert_eq!(rules(text), text, "input: {text}");
        }
    }

    #[test]
    fn prose_quotes_and_adjacent_scare_quotes_survive_full_pipeline() {
        let text = "Please quote the source and quote the page number.";
        assert_eq!(rules(text), text);
        assert_eq!(
            rules("in Anführungszeichen Hallo Anführungszeichen auf Welt Anführungszeichen zu"),
            "\"Hallo\" \"Welt\""
        );
    }
}
