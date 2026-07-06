//! The spoken-command → symbol substitution engine.
//!
//! Rules are matched case-insensitively over Unicode-aware word tokens (see
//! [`crate::text_rules::lex`]), support multi-word triggers, and are applied
//! longest-match-first. User rules are merged over the built-in table (same
//! trigger wins) and built-ins can be individually disabled.

use super::{builtin_rules, lex, SpacingPolicy, TextRule, Token};

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
                    pieces.push(Piece::Rule {
                        replacement: rule.replacement.clone(),
                        spacing: rule.spacing,
                    });
                    i = end;
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
                out.push_str(&text);
            }
            Piece::Rule {
                replacement,
                spacing,
            } => {
                trim_trailing_whitespace(&mut out);
                out.push_str(&replacement);
                match spacing {
                    SpacingPolicy::AttachLeft => {
                        out.push(' ');
                        skip_next_space = true;
                    }
                    SpacingPolicy::Glue | SpacingPolicy::Standalone => {
                        skip_next_space = true;
                    }
                }
            }
        }
    }

    // A trailing AttachLeft space (e.g. "hallo Komma" → "hallo, ") is not
    // meaningful; the pipeline appends its own trailing space when configured.
    trim_trailing_whitespace(&mut out);

    out
}

fn trim_trailing_whitespace(out: &mut String) {
    while out.ends_with([' ', '\t']) {
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
        // Brackets are glued (no spaces on either side) per the built-in table.
        assert_eq!(subst("wert Klammer auf x Klammer zu"), "wert(x)");
    }

    #[test]
    fn longest_match_first() {
        // "neue Zeile" (2 words) must win over any single-word match.
        assert_eq!(subst("eins neue Zeile zwei"), "eins\nzwei");
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
        assert_eq!(subst("a neuer Absatz b"), "a\n\nb");
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
}
