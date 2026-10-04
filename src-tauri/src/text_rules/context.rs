//! Command-vs-prose detection for ambiguous spoken command words.
//!
//! Words like `Punkt`, `Komma`, `dash` or `quote` are also ordinary nouns and
//! verbs. Replacing them unconditionally corrupts prose ("Der Punkt ist, dass…"
//! → "Der.ist, dass…"), so built-in commands only fire in command context.
//! Precision beats recall: a missed command costs one manual keystroke, a
//! corrupted sentence costs a re-read.
//!
//! The rules (applied per occurrence, on Parakeet-style output that already
//! carries the recognizer's own punctuation):
//!
//! - **Prose veto** ([`is_prose_use`]): the word directly before the command
//!   (whitespace only in between) is a determiner, possessive, contracted
//!   preposition or a few prepositions/pronouns that only precede nouns/verbs
//!   (`der`, `ein`, `einen`, `zum`, `um`, `the`, `a`, `to`, `we` …) → keep the
//!   word. The same holds across up to two attributive adjectives
//!   (`der springende Punkt`, `ein wirklich guter Punkt`, `a mad dash`):
//!   lowercase adjective candidates leading back to a determiner. English
//!   candidates include common uninflected adjectives and adjective suffixes.
//!   For [`Gate::Strict`](super::Gate::Strict) a sentence-initial
//!   adjective-shaped word also vetoes (`Guter Punkt.`).
//! - **Glue**, **Punct**, and **Strict** all require a positive clause signal:
//!   ASR punctuation or a text/line edge on either side, or a line-break
//!   command. Glue and punctuation also accept a nearby glue/path command or
//!   token; an independently signaled punctuation command can anchor a short
//!   command sequence (`Hallo Komma wie geht's Fragezeichen`). The substitution
//!   pass checks at most eight neighboring words without crossing punctuation.
//!   A comma after an opening greeting and a bounded two-word identifier
//!   joined by hyphen/underscore also provide a fragment boundary signal.
//!   Punct and Strict still require something to their left. Domain/file/version
//!   dots are decided earlier by the links pass.

use super::Token;

/// Words that, directly before a command word, mark it as a noun/verb.
const VETO_WORDS: &[&str] = &[
    // German articles, possessives (inflected only: bare "sein"/"ihr" are
    // also an infinitive / pronoun that often ends a sentence), demonstratives.
    "der",
    "die",
    "das",
    "den",
    "dem",
    "des",
    "ein",
    "eine",
    "einen",
    "einem",
    "einer",
    "eines",
    "kein",
    "keine",
    "keinen",
    "keinem",
    "keiner",
    "keines",
    "mein",
    "meine",
    "meinen",
    "meinem",
    "meiner",
    "meines",
    "dein",
    "deine",
    "deinen",
    "deinem",
    "deiner",
    "seine",
    "seinen",
    "seinem",
    "seiner",
    "ihre",
    "ihren",
    "ihrem",
    "ihrer",
    "unser",
    "unsere",
    "unseren",
    "unserem",
    "euer",
    "eure",
    "euren",
    "jeder",
    "jede",
    "jedes",
    "jeden",
    "jedem",
    "dieser",
    "diese",
    "dieses",
    "diesen",
    "diesem",
    "jener",
    "jene",
    "jenen",
    "welcher",
    "welche",
    "welchen",
    // German contracted prepositions + prepositions that take a noun here.
    "zum",
    "zur",
    "am",
    "im",
    "vom",
    "beim",
    "ins",
    "ans",
    "aufs",
    "um",
    "für",
    "bis",
    "pro",
    "ohne",
    "mit",
    "von",
    "zu",
    "nach",
    "über",
    "unter",
    "vor",
    "hinter",
    "neben",
    "zwischen",
    "durch",
    "gegen",
    "nächster",
    "neuer",
    // English determiners, possessives, verb markers.
    "the",
    "a",
    "an",
    "this",
    "that",
    "these",
    "those",
    "my",
    "your",
    "his",
    "her",
    "its",
    "our",
    "their",
    "no",
    "every",
    "each",
    "any",
    "some",
    "another",
    "next",
    "new",
    "to",
    "will",
    "would",
    "can",
    "could",
    "should",
    "must",
    "might",
    "shall",
    "i",
    "we",
    "you",
    "they",
    "he",
    "she",
];

/// Words that open a noun phrase (used when walking back over adjectives).
fn is_determiner(word: &str) -> bool {
    is_veto_word(word)
}

pub(crate) fn is_veto_word(word: &str) -> bool {
    VETO_WORDS.contains(&word.to_lowercase().as_str())
}

/// Intensifiers that may sit between a determiner and its adjective.
const INTENSIFIERS: &[&str] = &[
    "sehr",
    "wirklich",
    "ganz",
    "echt",
    "besonders",
    "ziemlich",
    "extrem",
    "very",
    "really",
    "quite",
    "extremely",
];

/// English adjectives do not share German case endings. Keep this compact to
/// avoid mistaking arbitrary verbs or e-mail names for attributive adjectives.
const ENGLISH_ADJECTIVES: &[&str] = &[
    "big", "small", "large", "little", "huge", "tiny", "red", "green", "blue", "black", "white",
    "mad", "good", "bad", "great", "quick", "slow", "short", "long", "old", "young", "hard",
    "soft", "high", "low", "hot", "cold", "full", "empty", "bold", "real", "final", "single",
    "double", "bright", "dark", "round", "sharp", "strange",
];

/// An inflected German adjective shape (`springende`, `guter`, `großes`); with
/// `allow_intensifier`, also an intensifier in front of one (`wirklich`).
fn is_adjective_shaped(word: &str, allow_intensifier: bool) -> bool {
    let lower = word.to_lowercase();
    if allow_intensifier && INTENSIFIERS.contains(&lower.as_str()) {
        return true;
    }
    if ENGLISH_ADJECTIVES.contains(&lower.as_str()) {
        return true;
    }
    lower.chars().count() >= 4
        && [
            "e", "en", "er", "es", "em", "ful", "less", "ous", "ive", "able", "ible", "al", "ic",
            "y",
        ]
        .iter()
        .any(|ending| lower.ends_with(ending))
}

fn starts_lowercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_lowercase)
}

pub(crate) fn starts_uppercase(word: &str) -> bool {
    word.chars().next().is_some_and(char::is_uppercase)
}

/// The prosody punctuation the recognizer adds (and the engine may absorb).
pub(crate) fn is_prosody_char(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | '…')
}

/// What lies directly before a token, ignoring plain spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Before {
    /// Start of text or a line break.
    Start,
    /// Punctuation or another symbol (an ASR segment boundary).
    Punct,
    /// A word at this token index, separated by plain whitespace only.
    Word(usize),
}

/// What lies directly after a token span, ignoring plain spaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum After {
    /// End of text or a line break.
    End,
    /// A prosody punctuation run.
    Punct,
    /// Some other symbol.
    Symbol,
    /// A word at this token index.
    Word(usize),
}

pub(crate) fn before(tokens: &[Token], start: usize) -> Before {
    let mut i = start;
    while i > 0 {
        i -= 1;
        match &tokens[i] {
            Token::Space(space) if space.contains('\n') => return Before::Start,
            Token::Space(_) => continue,
            Token::Word(_) => return Before::Word(i),
            Token::Other(_) => return Before::Punct,
        }
    }
    Before::Start
}

pub(crate) fn after(tokens: &[Token], end: usize) -> After {
    let mut i = end;
    while let Some(token) = tokens.get(i) {
        match token {
            Token::Space(space) if space.contains('\n') => return After::End,
            Token::Space(_) => i += 1,
            Token::Word(_) => return After::Word(i),
            Token::Other(text) => {
                return if text.starts_with(is_prosody_char) {
                    After::Punct
                } else {
                    After::Symbol
                };
            }
        }
    }
    After::End
}

/// Whether the command word at token `start` is used as a noun/verb in prose
/// (see the module docs). `strict` adds the sentence-initial adjective veto.
pub(crate) fn is_prose_use(tokens: &[Token], start: usize, strict: bool) -> bool {
    let Before::Word(prev) = before(tokens, start) else {
        return false;
    };
    let prev_word = tokens[prev].text();
    if is_veto_word(prev_word) {
        return true;
    }

    // Walk back over up to two attributive adjectives to a determiner.
    let mut idx = prev;
    for _ in 0..2 {
        let word = tokens[idx].text();
        if !is_adjective_shaped(word, idx != prev) {
            return false;
        }
        match before(tokens, idx) {
            Before::Word(p) => {
                // A capitalized word mid-sentence is a noun, not an adjective.
                if !starts_lowercase(word) {
                    return false;
                }
                if is_determiner(tokens[p].text()) {
                    return true;
                }
                idx = p;
            }
            // Sentence-initial adjective ("Guter Punkt."), any case.
            _ => return strict && idx == prev && is_adjective_shaped(word, false),
        }
    }
    false
}

/// Positive boundary signal, shared by punctuation and glue command gates.
pub(crate) fn has_clause_signal(tokens: &[Token], start: usize, end: usize) -> bool {
    matches!(before(tokens, start), Before::Start | Before::Punct)
        || matches!(after(tokens, end), After::End | After::Punct)
        || matches!(after(tokens, end), After::Word(next) if starts_line_break_command(tokens, next))
        || fragment_signal(tokens, start, end)
}

/// Keep established unpunctuated dictation forms with a bounded fragment
/// signal. Ambiguous English verbs (`slash`, `dash`) cannot get this signal
/// merely by sitting between two words in a short sentence.
fn fragment_signal(tokens: &[Token], start: usize, end: usize) -> bool {
    let Before::Word(left) = before(tokens, start) else {
        return false;
    };
    if !matches!(before(tokens, left), Before::Start | Before::Punct) {
        return false;
    }
    let command = tokens[start].text().to_lowercase();
    if matches!(command.as_str(), "komma" | "comma")
        && matches!(
            tokens[left].text().to_lowercase().as_str(),
            "hallo" | "hello" | "hi" | "hey" | "ende"
        )
    {
        return true;
    }
    let After::Word(right) = after(tokens, end) else {
        return false;
    };
    matches!(
        command.as_str(),
        "bindestrich" | "unterstrich" | "hyphen" | "underscore"
    ) && matches!(after(tokens, right + 1), After::End | After::Punct)
}

/// Maximum words searched per side for a neighboring command signal.
const NEIGHBOR_MAX_WORDS: usize = 8;

/// Nearby token indices, bounded to [`NEIGHBOR_MAX_WORDS`] per side in the
/// same clause.
pub(crate) fn neighbors(tokens: &[Token], start: usize, end: usize) -> Vec<usize> {
    let mut indices = Vec::new();
    for forward in [false, true] {
        let mut i = if forward { end } else { start };
        let mut words = 0;
        loop {
            if !forward {
                let Some(prev) = i.checked_sub(1) else { break };
                i = prev;
            }
            let Some(token) = tokens.get(i) else { break };
            match token {
                Token::Space(space) if space.contains('\n') => break,
                Token::Other(text) if text.chars().any(is_prosody_char) => break,
                Token::Word(_) => {
                    words += 1;
                    if words > NEIGHBOR_MAX_WORDS {
                        break;
                    }
                    indices.push(i);
                }
                Token::Other(_) => indices.push(i),
                _ => {}
            }
            if forward {
                i += 1;
            }
        }
    }
    indices
}

/// A written path token supplies the same signal as a spoken path command.
pub(crate) fn is_path_token(token: &Token) -> bool {
    matches!(token, Token::Other(text) if text.contains(['/', '\\', '~', '_', '@']))
        || matches!(token, Token::Word(text) if text.eq_ignore_ascii_case("cd") || text.eq_ignore_ascii_case("www"))
}

/// Whether the words starting at token `i` form a line-break command.
pub(crate) fn starts_line_break_command(tokens: &[Token], i: usize) -> bool {
    let Some(Token::Word(first)) = tokens.get(i) else {
        return false;
    };
    let After::Word(j) = after(tokens, i + 1) else {
        return false;
    };
    let pair = format!(
        "{} {}",
        first.to_lowercase(),
        tokens[j].text().to_lowercase()
    );
    matches!(
        pair.as_str(),
        "neue zeile" | "neuer absatz" | "new line" | "new paragraph"
    )
}

#[cfg(test)]
mod tests {
    use super::super::lex;
    use super::*;

    fn prose(text: &str, word: &str, strict: bool) -> bool {
        let tokens = lex(text);
        let i = tokens
            .iter()
            .position(|t| t.text() == word)
            .expect("word present");
        is_prose_use(&tokens, i, strict)
    }

    #[test]
    fn determiner_vetoes() {
        assert!(prose("Der Punkt ist", "Punkt", true));
        assert!(prose("Das ist ein Komma zu viel", "Komma", false));
        assert!(prose("Press the dash key", "dash", false));
    }

    #[test]
    fn adjective_walk_back_vetoes() {
        assert!(prose("der springende Punkt.", "Punkt", true));
        assert!(prose("ein wirklich guter Punkt.", "Punkt", true));
        assert!(prose("Guter Punkt.", "Punkt", true));
        assert!(prose("Click the red dot.", "dot", true));
        assert!(prose("I made a mad dash for the door.", "dash", false));
        assert!(prose(
            "There's a big question mark over the budget.",
            "question",
            false
        ));
    }

    #[test]
    fn german_prepositions_veto_command_nouns() {
        for preposition in [
            "mit", "von", "zu", "nach", "über", "unter", "vor", "hinter", "neben", "zwischen",
            "durch", "gegen",
        ] {
            assert!(prose(
                &format!("Das schreibt man {preposition} Bindestrich."),
                "Bindestrich",
                false
            ));
        }
    }

    #[test]
    fn verbs_and_nouns_before_command_do_not_veto() {
        assert!(!prose("wir noch warten müssen Punkt", "Punkt", true));
        assert!(!prose("das ist das Ende Punkt", "Punkt", true));
        assert!(!prose("ende Komma weiter", "Komma", false));
        assert!(!prose("Guten Tag, Punkt.", "Punkt", true));
    }
}
