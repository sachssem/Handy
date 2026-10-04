//! Per-app output styles (fork feature: voice-control). Deterministic, keyed
//! on the bundle id captured at dictation start (`dictation_context`), and
//! applied as the very last text stage — after learned corrections and the
//! self-correction LLM pass.
//!
//! Categories (each switchable; unknown apps stay unchanged):
//! - **chat** (Slack, Messages, WhatsApp, Telegram, Discord, Signal, Teams): a
//!   one-sentence message loses its single trailing period; `?`/`!` stay.
//! - **terminal** (Terminal, iTerm2, Warp, Ghostty, kitty, Alacritty,
//!   WezTerm): no trailing period, no ASR capital on the first word (same
//!   lowercase rule as below). Newlines are kept — TUI prompts (Claude Code)
//!   take multi-line input and shells receive a bracketed paste. The caret
//!   context is not trusted here (terminal AX text is the screen buffer), so
//!   context matching is off.
//! - **code** (VS Code, Cursor, Zed, JetBrains IDEs, Android Studio, Xcode,
//!   Sublime, Nova): a result that is a single token (identifier, path) loses
//!   its trailing period. Prose (commit messages, chat panels) is untouched.
//! - **mail/docs** (Mail, Outlook, Notes, Pages, Word): sentence style —
//!   unchanged. Browsers are not classified (the web app is unknown).
//!
//! Context matching (`match_context`, every app but terminals), from the text
//! before the caret:
//! - **Casing:** when that text ends mid-sentence (its last character is a
//!   letter, digit, `,` or `;` on the same line), the dictation's first word
//!   is lowercased — but only if it is a known German/English function word
//!   (article, pronoun, conjunction, preposition, auxiliary, particle). German
//!   capitalises nouns, so a blanket lowercase would be wrong; "Sie"/"Ihr"
//!   (formal address) are deliberately absent. Acronyms, words with inner
//!   capitals, "I" and capitalised words of the custom-word / learned
//!   dictionary are never touched. With a detected language the matching
//!   list is used, otherwise both.
//! - **Spacing:** when that text ends in a word or punctuation (`, . ; : ! ? )
//!   ] }`) without whitespace and the dictation starts with a word, a space is
//!   prepended. Upstream only appends a trailing space
//!   (`append_trailing_space`), which leaves whitespace before the caret, so
//!   the two never double up.
//!
//! Snippet expansions are protected: a text starting with one keeps its
//! casing, a text ending with one keeps its last character.

pub(crate) mod commands;

use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;

/// Which per-app styles are active.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
pub struct AppStyleCategories {
    #[serde(default = "crate::settings::default_true")]
    pub chat: bool,
    #[serde(default = "crate::settings::default_true")]
    pub terminal: bool,
    #[serde(default = "crate::settings::default_true")]
    pub code: bool,
    /// Casing and spacing matched to the text before the caret.
    #[serde(default = "crate::settings::default_true")]
    pub match_context: bool,
}

impl Default for AppStyleCategories {
    fn default() -> Self {
        Self {
            chat: true,
            terminal: true,
            code: true,
            match_context: true,
        }
    }
}

/// App categories with a distinct output style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppCategory {
    Chat,
    Terminal,
    Code,
    /// Mail and documents: sentence style, unchanged.
    Prose,
    Unknown,
}

const CHAT: &[&str] = &[
    "com.tinyspeck.slackmacgap",
    "com.apple.MobileSMS",
    "net.whatsapp.WhatsApp",
    "desktop.WhatsApp",
    "ru.keepcoder.Telegram",
    "org.telegram.desktop",
    "com.hnc.Discord",
    "org.whispersystems.signal-desktop",
    "com.microsoft.teams2",
    "com.microsoft.teams",
];
const TERMINAL: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "dev.warp.Warp",
    "com.mitchellh.ghostty",
    "net.kovidgoyal.kitty",
    "org.alacritty",
    "io.alacritty",
    "com.github.wez.wezterm",
];
const CODE: &[&str] = &[
    "com.microsoft.VSCode",
    "com.microsoft.VSCodeInsiders",
    "com.todesktop.230313mzl4w4u92", // Cursor
    "dev.zed.Zed",
    "dev.zed.Zed-Preview",
    "com.apple.dt.Xcode",
    "com.sublimetext.4",
    "com.sublimetext.3",
    "com.panic.Nova",
    "com.google.android.studio",
];
/// Bundle-id prefixes of code editors (every JetBrains IDE).
const CODE_PREFIXES: &[&str] = &["com.jetbrains."];
const PROSE: &[&str] = &[
    "com.apple.mail",
    "com.microsoft.Outlook",
    "com.apple.Notes",
    "com.apple.iWork.Pages",
    "com.microsoft.Word",
];

pub(crate) fn categorize(bundle_id: &str) -> AppCategory {
    let is = |list: &[&str]| list.iter().any(|id| id.eq_ignore_ascii_case(bundle_id));
    if is(CHAT) {
        AppCategory::Chat
    } else if is(TERMINAL) {
        AppCategory::Terminal
    } else if is(CODE) || CODE_PREFIXES.iter().any(|p| bundle_id.starts_with(p)) {
        AppCategory::Code
    } else if is(PROSE) {
        AppCategory::Prose
    } else {
        AppCategory::Unknown
    }
}

/// German words that are lowercase mid-sentence (never nouns). Formal
/// "Sie"/"Ihr"/"Ihnen" are deliberately absent.
const FUNCTION_WORDS_DE: &[&str] = &[
    "aber",
    "acht",
    "alle",
    "alles",
    "also",
    "am",
    "an",
    "auch",
    "auf",
    "aus",
    "bei",
    "bin",
    "bis",
    "bist",
    "bitte",
    "da",
    "dann",
    "das",
    "dass",
    "dein",
    "deine",
    "dem",
    "den",
    "denn",
    "der",
    "des",
    "dich",
    "die",
    "dir",
    "doch",
    "du",
    "durch",
    "ein",
    "eine",
    "einem",
    "einen",
    "einer",
    "eines",
    "er",
    "es",
    "etwas",
    "euch",
    "für",
    "gerade",
    "gibt",
    "hab",
    "habe",
    "haben",
    "hast",
    "hat",
    "hatte",
    "hier",
    "ich",
    "ihm",
    "ihn",
    "im",
    "in",
    "ist",
    "ja",
    "jetzt",
    "kann",
    "kannst",
    "können",
    "mal",
    "man",
    "mein",
    "meine",
    "meinem",
    "meinen",
    "mich",
    "mir",
    "mit",
    "muss",
    "müssen",
    "nach",
    "nein",
    "nicht",
    "nichts",
    "noch",
    "nur",
    "ob",
    "oder",
    "ohne",
    "schon",
    "sehr",
    "sein",
    "seine",
    "sind",
    "so",
    "soll",
    "sollte",
    "sondern",
    "um",
    "und",
    "uns",
    "unser",
    "unsere",
    "unter",
    "viel",
    "vielleicht",
    "von",
    "vor",
    "war",
    "waren",
    "was",
    "weil",
    "wenn",
    "wer",
    "werden",
    "wie",
    "wir",
    "wird",
    "wo",
    "wollte",
    "würde",
    "zu",
    "zum",
    "zur",
    "über",
];

/// English words that are lowercase mid-sentence. "I" is never lowercased.
const FUNCTION_WORDS_EN: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "are", "as", "at", "be", "because", "been",
    "but", "by", "can", "could", "did", "do", "does", "for", "from", "had", "has", "have", "he",
    "her", "his", "how", "if", "in", "is", "it", "its", "just", "maybe", "me", "my", "no", "not",
    "of", "on", "or", "our", "please", "she", "so", "that", "the", "their", "them", "then",
    "there", "these", "they", "this", "those", "to", "too", "us", "was", "we", "were", "what",
    "when", "where", "which", "while", "who", "why", "will", "with", "would", "yes", "you", "your",
];

/// Whether the first word `word` (as dictated, capitalised by the ASR) may be
/// lowercased. See the module docs.
fn may_lowercase(word: &str, lang: Option<&str>, dictionary: &[String]) -> bool {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    // Only a plain capitalised word: no acronym, no inner capital (iPhone,
    // GitHub), no "I" / "I'm".
    if !first.is_uppercase() || chars.any(char::is_uppercase) || word == "I" {
        return false;
    }
    if dictionary
        .iter()
        .flat_map(|entry| entry.split_whitespace())
        .any(|entry| entry == word)
    {
        return false;
    }
    let lower = word.to_lowercase();
    let in_de = FUNCTION_WORDS_DE.contains(&lower.as_str());
    let in_en = FUNCTION_WORDS_EN.contains(&lower.as_str());
    match lang {
        Some("de") => in_de,
        Some("en") => in_en,
        None => in_de || in_en,
        Some(_) => false,
    }
}

/// Lowercase the first word of `text` when [`may_lowercase`] allows it.
fn lowercase_first_word(text: &str, lang: Option<&str>, dictionary: &[String]) -> String {
    let start = text.len() - text.trim_start().len();
    let rest = &text[start..];
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '\''))
        .unwrap_or(rest.len());
    let word = &rest[..end];
    // "I'm" and other apostrophe forms of "I" stay.
    if word.starts_with("I'") || !may_lowercase(word, lang, dictionary) {
        return text.to_string();
    }
    let mut chars = word.chars();
    let first: String = chars
        .next()
        .map(|c| c.to_lowercase().collect())
        .unwrap_or_default();
    format!(
        "{}{}{}{}",
        &text[..start],
        first,
        chars.as_str(),
        &rest[end..]
    )
}

/// `text` without its single trailing period (an ellipsis stays).
fn strip_trailing_period(text: &str) -> Option<String> {
    let trimmed = text.trim_end();
    let without = trimmed.strip_suffix('.')?;
    if without.ends_with('.') || without.trim_end().is_empty() {
        return None;
    }
    Some(without.to_string())
}

/// One sentence on one line: no `.`/`!`/`?` followed by whitespace before the
/// end, no line break.
fn is_single_sentence(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.contains('\n') {
        return false;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    !chars
        .windows(2)
        .any(|w| matches!(w[0], '.' | '!' | '?') && w[1].is_whitespace())
}

/// The text before the caret ends mid-sentence on the current line.
fn ends_mid_sentence(before: &str) -> bool {
    let trimmed = before.trim_end();
    if before[trimmed.len()..].contains('\n') {
        return false;
    }
    trimmed
        .chars()
        .last()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, ',' | ';'))
}

/// A space is needed between the text before the caret and the dictation.
fn needs_leading_space(before: &str, text: &str) -> bool {
    let joins_after = before.chars().last().is_some_and(|c| {
        c.is_alphanumeric() || matches!(c, ',' | '.' | ';' | ':' | '!' | '?' | ')' | ']' | '}')
    });
    joins_after && text.chars().next().is_some_and(char::is_alphanumeric)
}

/// Where the dictation goes.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Target<'a> {
    pub bundle_id: Option<&'a str>,
    pub text_before_caret: Option<&'a str>,
}

/// Apply the per-app style to `text`. `lang` is the transcription's base
/// language (if known), `dictionary` the user's custom / learned words,
/// `protected` the byte ranges of actual snippet insertions in `text`.
pub(crate) fn style(
    text: &str,
    target: Target,
    toggles: &AppStyleCategories,
    lang: Option<&str>,
    dictionary: &[String],
    protected: &[Range<usize>],
) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let category = target
        .bundle_id
        .map(categorize)
        .unwrap_or(AppCategory::Unknown);
    let first = text.len() - text.trim_start().len();
    let last = text.trim_end().len();
    let starts_protected = protected.iter().any(|p| p.contains(&first));
    let ends_protected = last > 0 && protected.iter().any(|p| p.contains(&(last - 1)));
    let mut out = text.to_string();

    let strip_period = !ends_protected
        && match category {
            AppCategory::Chat => toggles.chat && is_single_sentence(&out),
            AppCategory::Terminal => toggles.terminal,
            AppCategory::Code => toggles.code && !out.trim().contains(char::is_whitespace),
            AppCategory::Prose | AppCategory::Unknown => false,
        };
    if strip_period {
        if let Some(stripped) = strip_trailing_period(&out) {
            out = stripped;
        }
    }

    if category == AppCategory::Terminal {
        if toggles.terminal && !starts_protected {
            out = lowercase_first_word(&out, lang, dictionary);
        }
        return out;
    }

    if toggles.match_context {
        if let Some(before) = target.text_before_caret.filter(|b| !b.is_empty()) {
            if !starts_protected && ends_mid_sentence(before) {
                out = lowercase_first_word(&out, lang, dictionary);
            }
            if needs_leading_space(before, &out) {
                out.insert(0, ' ');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: AppStyleCategories = AppStyleCategories {
        chat: true,
        terminal: true,
        code: true,
        match_context: true,
    };

    fn in_app(bundle_id: &str, text: &str) -> String {
        style(
            text,
            Target {
                bundle_id: Some(bundle_id),
                text_before_caret: None,
            },
            &ALL,
            None,
            &[],
            &[],
        )
    }

    fn after(before: &str, text: &str, lang: Option<&str>) -> String {
        style(
            text,
            Target {
                bundle_id: Some("com.apple.Notes"),
                text_before_caret: Some(before),
            },
            &ALL,
            lang,
            &["Und".to_string()],
            &[],
        )
    }

    #[test]
    fn categorizes_bundle_ids() {
        assert_eq!(categorize("com.tinyspeck.slackmacgap"), AppCategory::Chat);
        assert_eq!(categorize("com.googlecode.iterm2"), AppCategory::Terminal);
        assert_eq!(categorize("com.jetbrains.intellij"), AppCategory::Code);
        assert_eq!(categorize("com.apple.mail"), AppCategory::Prose);
        assert_eq!(categorize("com.google.Chrome"), AppCategory::Unknown);
    }

    #[test]
    fn chat_drops_the_period_of_one_sentence_only() {
        let slack = "com.tinyspeck.slackmacgap";
        assert_eq!(
            in_app(slack, "Bin in fünf Minuten da."),
            "Bin in fünf Minuten da"
        );
        assert_eq!(in_app(slack, "Kommst du mit?"), "Kommst du mit?");
        assert_eq!(in_app(slack, "Super!"), "Super!");
        assert_eq!(in_app(slack, "Mal sehen..."), "Mal sehen...");
        assert_eq!(in_app(slack, "Ja. Bin gleich da."), "Ja. Bin gleich da.");
        assert_eq!(in_app(slack, "Liste:\n- Milch."), "Liste:\n- Milch.");
    }

    #[test]
    fn terminal_drops_period_and_function_word_capital() {
        let term = "com.apple.Terminal";
        assert_eq!(in_app(term, "Git status."), "Git status");
        assert_eq!(
            in_app(term, "Was macht dieser Befehl."),
            "was macht dieser Befehl"
        );
        assert_eq!(
            in_app(term, "The build failed.\nWhy?"),
            "the build failed.\nWhy?"
        );
        assert_eq!(in_app(term, "Datei löschen."), "Datei löschen");
    }

    #[test]
    fn code_drops_the_period_of_a_single_token() {
        let code = "com.microsoft.VSCode";
        assert_eq!(in_app(code, "getUserName."), "getUserName");
        assert_eq!(in_app(code, "src/main.rs."), "src/main.rs");
        assert_eq!(in_app(code, "Fix the parser."), "Fix the parser.");
    }

    #[test]
    fn unknown_and_prose_apps_are_unchanged() {
        assert_eq!(in_app("com.google.Chrome", "Hallo."), "Hallo.");
        assert_eq!(in_app("com.apple.mail", "Hallo."), "Hallo.");
    }

    #[test]
    fn mid_sentence_continuation_lowercases_function_words_only() {
        assert_eq!(
            after("Ich komme morgen,", "Weil ich Zeit habe.", None),
            " weil ich Zeit habe."
        );
        assert_eq!(
            after("Ich komme morgen, ", "Weil ich.", Some("de")),
            "weil ich."
        );
        // German noun, formal address, acronym, "I", dictionary word stay.
        assert_eq!(after("Wir brauchen ", "Kaffee.", Some("de")), "Kaffee.");
        assert_eq!(after("Danke, ", "Sie sind toll.", None), "Sie sind toll.");
        assert_eq!(after("Check the ", "API docs.", None), "API docs.");
        assert_eq!(after("and then ", "I left.", Some("en")), "I left.");
        assert_eq!(after("Tom ", "Und Jerry.", None), "Und Jerry.");
        // English list only for English.
        assert_eq!(after("and ", "The rest.", Some("en")), "the rest.");
        assert_eq!(after("und ", "The rest.", Some("de")), "The rest.");
    }

    #[test]
    fn sentence_end_or_new_line_keeps_the_capital() {
        assert_eq!(after("Fertig. ", "Weil ich.", None), "Weil ich.");
        assert_eq!(after("Hallo Marc,\n\n", "Weil ich.", None), "Weil ich.");
        assert_eq!(after("", "Weil ich.", None), "Weil ich.");
    }

    #[test]
    fn prepends_a_space_only_when_glued() {
        assert_eq!(after("Hallo", "Welt.", Some("de")), " Welt.");
        assert_eq!(after("Ende.", "Neu.", None), " Neu.");
        assert_eq!(after("Hallo ", "Welt.", None), "Welt.");
        assert_eq!(after("(", "Welt.", None), "Welt.");
    }

    #[test]
    fn snippet_expansions_are_protected() {
        let link = "https://calendly.com/marc.";
        let out = style(
            link,
            Target {
                bundle_id: Some("com.tinyspeck.slackmacgap"),
                text_before_caret: Some("Hier, "),
            },
            &ALL,
            None,
            &[],
            std::slice::from_ref(&(0..link.len())),
        );
        assert_eq!(out, link);
    }

    #[test]
    fn coincidental_expansion_at_another_edge_is_not_protected() {
        let text = "The rest. The rest.";
        let out = style(
            text,
            Target {
                bundle_id: Some("com.apple.Notes"),
                text_before_caret: Some("and "),
            },
            &ALL,
            Some("en"),
            &[],
            std::slice::from_ref(&(10..19)),
        );
        assert_eq!(out, "the rest. The rest.");
    }

    #[test]
    fn disabled_toggles_leave_text_alone() {
        let none = AppStyleCategories {
            chat: false,
            terminal: false,
            code: false,
            match_context: false,
        };
        let out = style(
            "Weil ich da bin.",
            Target {
                bundle_id: Some("com.tinyspeck.slackmacgap"),
                text_before_caret: Some("Ja,"),
            },
            &none,
            None,
            &[],
            &[],
        );
        assert_eq!(out, "Weil ich da bin.");
    }
}
