//! The spoken-command → symbol substitution engine.
//!
//! Rules are matched case-insensitively over Unicode-aware word tokens (see
//! [`crate::text_rules::lex`]), support multi-word triggers, and are applied
//! longest-match-first. User rules are merged over the built-in table (same
//! trigger wins) and built-ins can be individually disabled.
//!
//! The engine is hardened for real ASR output, where the recognizer sprinkles
//! its own prosody punctuation around spoken commands (e.g. dictating
//! "Guten Tag Punkt Hallo" yields `Guten Tag, Punkt. Hallo`). To keep the
//! result clean it:
//! - **absorbs** ASR punctuation (`. , ; : ! ? …`) that hugs a matched trigger
//!   span (immediately before the first / after the last trigger token, across
//!   whitespace) — those marks are pause artifacts, not content;
//! - lets an [`SpacingPolicy::AttachLeft`] replacement **replace** a trailing
//!   punctuation run already in the output (the explicit command wins over the
//!   ASR guess); and
//! - **re-capitalizes** the next word after a replacement that ends a sentence
//!   (`.` `!` `?`) or starts a new line.

use super::{builtin_rules, lex, SpacingPolicy, TextRule, Token};

/// The punctuation characters treated as ASR prosody artifacts: sentence and
/// clause marks plus the Unicode ellipsis. An ASCII ellipsis `...` is a run of
/// `.` and is covered by the run-based checks below.
const PUNCT_CHARS: [char; 7] = ['.', ',', ';', ':', '!', '?', '…'];

fn is_punct_char(c: char) -> bool {
    PUNCT_CHARS.contains(&c)
}

/// Whether `text` is a non-empty run consisting solely of prosody punctuation
/// (so it can be absorbed wholesale, e.g. `","`, `"."`, `"..."`, `"…"`).
fn is_punct_run(text: &str) -> bool {
    !text.is_empty() && text.chars().all(is_punct_char)
}

/// A closing bracket keeps punctuation that follows it: after `)` a list comma
/// or sentence period is legitimate content, not an ASR artifact.
fn keeps_following_punct(replacement: &str) -> bool {
    matches!(replacement.chars().last(), Some(')' | ']' | '}'))
}

/// A rule compiled for matching: the trigger split into lowercased word parts
/// plus the replacement and spacing policy.
struct CompiledRule {
    words: Vec<String>,
    replacement: String,
    spacing: SpacingPolicy,
}

/// An intermediate output piece produced by the matcher and rendered into the
/// final string with the correct spacing.
enum Piece {
    /// Verbatim passthrough text (an unmatched word or existing punctuation).
    Verbatim(String),
    /// Original whitespace run (may be dropped by an adjacent rule).
    Space(String),
    /// A matched rule's replacement together with its spacing policy.
    Rule {
        replacement: String,
        spacing: SpacingPolicy,
        /// Whether a trailing ASR punctuation mark was absorbed right after the
        /// trigger. It flips a glued replacement (e.g. `Punkt` → `.`) into a
        /// sentence terminator that keeps its following space.
        absorbed_following: bool,
    },
}

/// Apply the substitution engine to `text`.
///
/// `custom_rules` are user-authored rules merged over the built-ins; a custom
/// rule whose trigger (case-insensitively) equals a built-in's replaces it.
/// `disabled_builtins` lists built-in triggers to omit entirely.
pub fn apply_substitutions(
    text: &str,
    custom_rules: &[TextRule],
    disabled_builtins: &[String],
) -> String {
    let rules = compile_rules(custom_rules, disabled_builtins);
    if rules.is_empty() {
        return text.to_string();
    }

    let tokens = lex(text);
    let pieces = match_tokens(&tokens, &rules);
    render(pieces)
}

/// Merge built-ins and user rules into a single match table, longest-first.
fn compile_rules(custom_rules: &[TextRule], disabled_builtins: &[String]) -> Vec<CompiledRule> {
    let disabled: Vec<String> = disabled_builtins.iter().map(|t| t.to_lowercase()).collect();

    let mut compiled: Vec<CompiledRule> = Vec::new();

    for rule in builtin_rules() {
        if disabled.contains(&rule.trigger.to_lowercase()) {
            continue;
        }
        if let Some(compiled_rule) = compile_rule(&rule) {
            compiled.push(compiled_rule);
        }
    }

    // User rules override built-ins with the same trigger, and add new ones.
    for rule in custom_rules {
        if let Some(compiled_rule) = compile_rule(rule) {
            compiled.retain(|existing| existing.words != compiled_rule.words);
            compiled.push(compiled_rule);
        }
    }

    // Longest-match-first: more trigger words first, then longer triggers.
    compiled.sort_by(|a, b| {
        b.words
            .len()
            .cmp(&a.words.len())
            .then_with(|| trigger_len(b).cmp(&trigger_len(a)))
    });

    compiled
}

fn trigger_len(rule: &CompiledRule) -> usize {
    rule.words.iter().map(|w| w.chars().count()).sum()
}

fn compile_rule(rule: &TextRule) -> Option<CompiledRule> {
    let words: Vec<String> = rule
        .trigger
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();

    if words.is_empty() {
        return None;
    }

    Some(CompiledRule {
        words,
        replacement: rule.replacement.clone(),
        spacing: rule.spacing,
    })
}

/// Walk the token stream, replacing matched trigger phrases with rule pieces.
fn match_tokens(tokens: &[Token], rules: &[CompiledRule]) -> Vec<Piece> {
    let mut pieces = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        match &tokens[i] {
            Token::Word(word) => {
                if let Some((rule, end)) = rules
                    .iter()
                    .find_map(|rule| try_match(tokens, i, rule).map(|end| (rule, end)))
                {
                    // Absorb ASR punctuation that hugs the matched trigger span.
                    absorb_preceding(&mut pieces);
                    let (next, absorbed_following) = if keeps_following_punct(&rule.replacement) {
                        (end, false)
                    } else {
                        absorb_following(tokens, end)
                    };
                    pieces.push(Piece::Rule {
                        replacement: rule.replacement.clone(),
                        spacing: rule.spacing,
                        absorbed_following,
                    });
                    i = next;
                } else {
                    pieces.push(Piece::Verbatim(word.clone()));
                    i += 1;
                }
            }
            Token::Space(space) => {
                pieces.push(Piece::Space(space.clone()));
                i += 1;
            }
            Token::Other(other) => {
                pieces.push(Piece::Verbatim(other.clone()));
                i += 1;
            }
        }
    }

    pieces
}

/// Drop already-emitted punctuation that immediately precedes a matched trigger
/// (any run of prosody punctuation reachable across whitespace, stopping at the
/// first real word or previous replacement). These are ASR pause artifacts.
fn absorb_preceding(pieces: &mut Vec<Piece>) {
    loop {
        while matches!(pieces.last(), Some(Piece::Space(_))) {
            pieces.pop();
        }
        match pieces.last() {
            Some(Piece::Verbatim(text)) if is_punct_run(text) => {
                pieces.pop();
            }
            _ => break,
        }
    }
}

/// Consume punctuation tokens that immediately follow a matched trigger (across
/// whitespace, stopping at the first real word). Returns the token index to
/// resume from and whether anything was absorbed.
fn absorb_following(tokens: &[Token], end: usize) -> (usize, bool) {
    let mut i = end;
    let mut absorbed = false;

    loop {
        let mut probe = i;
        while matches!(tokens.get(probe), Some(Token::Space(_))) {
            probe += 1;
        }
        match tokens.get(probe) {
            Some(Token::Other(text)) if is_punct_run(text) => {
                absorbed = true;
                i = probe + 1;
            }
            _ => break,
        }
    }

    (i, absorbed)
}

/// Try to match `rule` starting exactly at token index `start` (which must be a
/// [`Token::Word`]). Returns the token index just past the matched phrase.
fn try_match(tokens: &[Token], start: usize, rule: &CompiledRule) -> Option<usize> {
    let mut ti = start;

    for (idx, trigger_word) in rule.words.iter().enumerate() {
        match tokens.get(ti) {
            Some(Token::Word(word)) if word.to_lowercase() == *trigger_word => {}
            _ => return None,
        }
        ti += 1;

        // Between two trigger words there must be only whitespace.
        if idx + 1 < rule.words.len() {
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

/// Render the matched pieces into the final string, applying spacing policies.
fn render(pieces: Vec<Piece>) -> String {
    let mut out = String::new();
    // When a rule suppresses the space that follows it, we drop the next Space.
    let mut skip_next_space = false;
    // Set after a sentence-ending / newline replacement so the next word is
    // re-capitalized across that boundary.
    let mut capitalize_next = false;

    for piece in pieces {
        match piece {
            Piece::Space(space) => {
                if skip_next_space {
                    skip_next_space = false;
                } else {
                    out.push_str(&space);
                }
            }
            Piece::Verbatim(text) => {
                skip_next_space = false;
                push_verbatim(&mut out, &text, &mut capitalize_next);
            }
            Piece::Rule {
                replacement,
                spacing,
                absorbed_following,
            } => {
                trim_trailing_whitespace(&mut out);
                if spacing == SpacingPolicy::AttachLeft {
                    // The explicit command overrides any ASR punctuation guess
                    // already sitting at the end of the output.
                    trim_trailing_punctuation(&mut out);
                    trim_trailing_whitespace(&mut out);
                }
                if spacing == SpacingPolicy::AttachRight && !out.is_empty() {
                    out.push(' ');
                }
                out.push_str(&replacement);
                if space_after(spacing, absorbed_following) {
                    out.push(' ');
                }
                skip_next_space = true;
                capitalize_next = starts_new_sentence(&replacement, spacing, absorbed_following);
            }
        }
    }

    // A trailing AttachLeft space (e.g. "hallo Komma" → "hallo, ") is not
    // meaningful; the pipeline appends its own trailing space when configured.
    trim_trailing_whitespace(&mut out);

    out
}

/// Whether a replacement is followed by a single space given its policy. A
/// glued replacement gains a space only when it absorbed a trailing ASR mark,
/// which reveals it as a clause boundary (prose) rather than a joiner (paths).
fn space_after(spacing: SpacingPolicy, absorbed_following: bool) -> bool {
    match spacing {
        SpacingPolicy::AttachLeft => true,
        SpacingPolicy::Glue => absorbed_following,
        SpacingPolicy::AttachRight | SpacingPolicy::Standalone => false,
    }
}

/// Whether the replacement opens a new sentence, so the next word should be
/// capitalized. True for a terminal `.` `!` `?` that is followed by a space and
/// for any replacement ending in a newline.
fn starts_new_sentence(
    replacement: &str,
    spacing: SpacingPolicy,
    absorbed_following: bool,
) -> bool {
    if replacement.ends_with('\n') {
        return true;
    }
    let ends_terminal = matches!(replacement.chars().last(), Some('.' | '!' | '?'));
    ends_terminal && space_after(spacing, absorbed_following)
}

/// Append verbatim text, capitalizing its first alphabetic character when a
/// preceding replacement opened a new sentence. Unicode-aware (handles umlauts).
fn push_verbatim(out: &mut String, text: &str, capitalize_next: &mut bool) {
    if !*capitalize_next {
        out.push_str(text);
        return;
    }
    let mut resolved = false;
    for c in text.chars() {
        if !resolved && c.is_alphabetic() {
            if c.is_lowercase() {
                out.extend(c.to_uppercase());
            } else {
                out.push(c);
            }
            resolved = true;
        } else {
            out.push(c);
        }
    }
    if resolved {
        *capitalize_next = false;
    }
}

fn trim_trailing_whitespace(out: &mut String) {
    while out.ends_with([' ', '\t']) {
        out.pop();
    }
}

fn trim_trailing_punctuation(out: &mut String) {
    while out.chars().next_back().is_some_and(is_punct_char) {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subst(text: &str) -> String {
        apply_substitutions(text, &[], &[])
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(subst("ende KOMMA weiter"), "ende, weiter");
        assert_eq!(subst("ende komma weiter"), "ende, weiter");
    }

    #[test]
    fn multi_word_trigger() {
        // "Klammer auf" hugs the word to its right (AttachRight); "Klammer zu"
        // glues to the word on its left.
        assert_eq!(subst("wert Klammer auf x Klammer zu"), "wert (x)");
    }

    #[test]
    fn longest_match_first() {
        // "neue Zeile" (2 words) must win over any single-word match. The word
        // after the newline is re-capitalized (new line starts a sentence).
        assert_eq!(subst("eins neue Zeile zwei"), "eins\nZwei");
    }

    #[test]
    fn umlaut_adjacent_boundary() {
        // "Bindestrichen" must NOT match the trigger "Bindestrich".
        assert_eq!(
            subst("viele Bindestrichen hier"),
            "viele Bindestrichen hier"
        );
    }

    #[test]
    fn glue_policy() {
        assert_eq!(subst("voice Bindestrich control"), "voice-control");
    }

    #[test]
    fn attach_left_policy() {
        assert_eq!(subst("hallo Komma welt"), "hallo, welt");
    }

    #[test]
    fn standalone_policy() {
        // A new paragraph starts a sentence, so "b" is re-capitalized.
        assert_eq!(subst("a neuer Absatz b"), "a\n\nB");
    }

    #[test]
    fn user_override_of_builtin() {
        let rules = vec![TextRule {
            trigger: "Komma".to_string(),
            replacement: ";".to_string(),
            spacing: SpacingPolicy::AttachLeft,
        }];
        assert_eq!(apply_substitutions("a Komma b", &rules, &[]), "a; b");
    }

    #[test]
    fn disabled_builtin() {
        let disabled = vec!["Punkt".to_string()];
        assert_eq!(
            apply_substitutions("www Punkt example", &[], &disabled),
            "www Punkt example"
        );
    }

    #[test]
    fn user_rule_new_trigger() {
        let rules = vec![TextRule {
            trigger: "smiley".to_string(),
            replacement: ":)".to_string(),
            spacing: SpacingPolicy::Standalone,
        }];
        assert_eq!(apply_substitutions("ende smiley", &rules, &[]), "ende:)");
    }

    // --- ASR-realistic hardening cases (Parakeet v3 output). ---------------
    //
    // The recognizer wraps spoken commands in its own prosody punctuation; the
    // engine must absorb those marks instead of stacking them.

    #[test]
    fn absorbs_prosody_around_terminal() {
        // The comma after "Tag" and the period after "Punkt" are ASR artifacts.
        assert_eq!(
            subst("Guten Tag, Punkt. Hallo, wie geht's dir?"),
            "Guten Tag. Hallo, wie geht's dir?"
        );
    }

    #[test]
    fn terminal_then_newline() {
        assert_eq!(
            subst("Guten Tag, hallo, Punkt. Neue Zeile. Wie geht es dir?"),
            "Guten Tag, hallo.\nWie geht es dir?"
        );
    }

    #[test]
    fn brackets_in_prose() {
        // Opening bracket hugs the following word; the closing bracket keeps the
        // real list comma that follows it.
        assert_eq!(
            subst("So, Test, test, Klammer auf, mir geht's ganz gut, Klammer zu, wie geht's dir?"),
            "So, Test, test (mir geht's ganz gut), wie geht's dir?"
        );
    }

    #[test]
    fn absorbs_trailing_after_final_trigger() {
        // Decision: a punctuation mark directly attached after a final trigger is
        // treated as an ASR artifact and absorbed (not preserved as `Wort:.`).
        assert_eq!(subst("Wort, Doppelpunkt."), "Wort:");
    }

    #[test]
    fn newline_between_sentences() {
        assert_eq!(subst("test, Neue Zeile. Test."), "test\nTest.");
    }

    #[test]
    fn capitalizes_after_period() {
        assert_eq!(subst("eins, Punkt, zwei"), "eins. Zwei");
    }

    #[test]
    fn capitalizes_umlaut_after_period() {
        // Capitalization is Unicode-aware.
        assert_eq!(subst("gut, Punkt. übrigens"), "gut. Übrigens");
    }

    #[test]
    fn glue_period_keeps_path_semantics() {
        // Without an absorbed ASR mark, "Punkt" stays a glue joiner for paths.
        assert_eq!(subst("www Punkt example Punkt com"), "www.example.com");
    }

    #[test]
    fn german_question_mark_command_absorbs_prosody() {
        // The comma before "Fragezeichen" (attached to "hier") and the trailing
        // period are ASR artifacts; the AttachLeft "?" overrides the comma.
        assert_eq!(
            subst("So, das ist echt eine gute Frage hier, Fragezeichen."),
            "So, das ist echt eine gute Frage hier?"
        );
    }

    #[test]
    fn english_question_mark_command_absorbs_prosody() {
        // Multi-word English trigger, case-insensitive, same absorption.
        assert_eq!(
            subst("Echt eine super Frage hier, Question Mark."),
            "Echt eine super Frage hier?"
        );
    }
}
