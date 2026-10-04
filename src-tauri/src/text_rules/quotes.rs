//! Spoken quotation marks → paired `"…"`.
//!
//! Recognized forms (German key [`KEY_DE`], English key [`KEY_EN`]):
//! - explicit: `Anführungszeichen auf|oben … Anführungszeichen zu|unten|ende`,
//!   `open|begin quote … close|end quote` / `unquote`;
//! - paired: `Anführungszeichen … Anführungszeichen` (also
//!   `Anführungsstriche`, `Gänsefüßchen`); bare English `quote` requires an
//!   explicit `end quote`, `unquote`, or `close quote` closer;
//! - scare quotes: `in Anführungszeichen X` quotes the next word — or, when it
//!   is capitalized, the run of capitalized words (at most
//!   [`SCARE_QUOTE_MAX_WORDS`]) up to the next punctuation or quote marker —
//!   and drops the spoken `in`
//!   (`Wir nennen das in Anführungszeichen Feature Freeze` →
//!   `Wir nennen das "Feature Freeze"`). A `in` right before a paired opener is
//!   dropped the same way.
//!
//! An ambiguous word (`Anführungszeichen`, `quote`) only becomes a quote mark
//! when it pairs with a later closer; a lone one, or one after a determiner
//! (`die Anführungszeichen`, `a quote`), stays a word. An explicit opener is
//! emitted even unpaired; a closer without an opener stays a word.
//! Bare German markers followed by an article/determiner remain prose, and
//! any pair involving an ambiguous marker encloses at most eight words.
//!
//! Spacing: the opening mark takes a space before it and hugs the next word;
//! the closing mark hugs the previous word and keeps whatever follows. The
//! recognizer's prosody commas around the marks are dropped (`Er sagte:` keeps
//! its colon).
//! Adjacent ASR straight/curly marks on resolved commands are absorbed. An
//! ASR closing mark's sentence stop keeps its side of the quote, and a repeated
//! stop after the spoken closer is dropped. A sentence ending in `.`, `!` or
//! `?` inside a generated quote starts with a capital letter; fragments keep
//! their casing. Quotes without commands stay intact.
//!
//! The mark is [`QUOTE_MARK`], a straight ASCII `"`: dictation lands in code,
//! terminals and chat as often as in prose, and the straight quote is correct
//! (or auto-converted) everywhere, whereas German `„…“` breaks code.

use super::context::{self, After, Before};
use super::{builtin_active, lex, TextRule, Token};

pub(crate) const KEY_DE: &str = "Anführungszeichen";
pub(crate) const KEY_EN: &str = "quote";

/// The quotation mark emitted for both ends.
const QUOTE_MARK: char = '"';

/// Maximum capitalized words quoted by `in Anführungszeichen X`.
const SCARE_QUOTE_MAX_WORDS: usize = 4;
/// Maximum words enclosed by a pair involving an ambiguous quote marker.
const AMBIGUOUS_MAX_WORDS: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Open,
    Close,
    Ambiguous,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    /// Leave the words untouched.
    Keep,
    Open,
    Close,
    /// `in Anführungszeichen X`: open here, close after the scare-quoted span.
    Scare,
}

#[derive(Debug, Clone, Copy)]
struct Marker {
    /// First token of the marker word(s).
    start: usize,
    /// Token index just past the marker.
    end: usize,
    kind: Kind,
    /// Bare English `quote` can open only before an explicit closer.
    english: bool,
    /// Retained as a span boundary even when the marker is ordinary prose.
    vetoed: bool,
    /// Index of a directly preceding `in` (German) that is dropped when the
    /// marker opens a quote.
    in_word: Option<usize>,
    role: Role,
}

/// Apply quote pairing to `text`.
pub fn apply_quotes(text: &str, custom: &[TextRule], disabled: &[String]) -> String {
    let de = builtin_active(KEY_DE, custom, disabled);
    let en = builtin_active(KEY_EN, custom, disabled);
    if !de && !en {
        return text.to_string();
    }

    // Inspect ASR quotes independently of adjacent punctuation (`."`, `".`).
    let tokens: Vec<_> = lex(text)
        .into_iter()
        .flat_map(|token| match token {
            Token::Other(text) => text.chars().map(|c| Token::Other(c.to_string())).collect(),
            token => vec![token],
        })
        .collect();
    let mut markers = find_markers(&tokens, de, en);
    if markers.is_empty() {
        return text.to_string();
    }
    resolve_roles(&tokens, &mut markers);
    render(&tokens, &markers)
}

fn find_markers(tokens: &[Token], de: bool, en: bool) -> Vec<Marker> {
    let mut markers = Vec::new();
    let mut i = 0;

    while i < tokens.len() {
        let Token::Word(word) = &tokens[i] else {
            i += 1;
            continue;
        };
        let lower = word.to_lowercase();
        let next = match context::after(tokens, i + 1) {
            After::Word(j) => Some((j, tokens[j].text().to_lowercase())),
            _ => None,
        };
        let prev = context::before(tokens, i);

        let found = if de
            && matches!(
                lower.as_str(),
                "anführungszeichen" | "anführungsstriche" | "gänsefüßchen"
            ) {
            match next.as_ref().map(|(j, w)| (*j, w.as_str())) {
                Some((j, "auf" | "oben")) => Some((i, j + 1, Kind::Open)),
                Some((j, "zu" | "unten" | "ende")) => Some((i, j + 1, Kind::Close)),
                _ => Some((i, i + 1, Kind::Ambiguous)),
            }
        } else if en && lower == "unquote" {
            Some((i, i + 1, Kind::Close))
        } else if en && matches!(lower.as_str(), "open" | "begin" | "close" | "end") {
            match next.as_ref().map(|(j, w)| (*j, w.as_str())) {
                Some((j, "quote")) => {
                    let kind = if matches!(lower.as_str(), "open" | "begin") {
                        Kind::Open
                    } else {
                        Kind::Close
                    };
                    Some((i, j + 1, kind))
                }
                _ => None,
            }
        } else if en && lower == "quote" {
            Some((i, i + 1, Kind::Ambiguous))
        } else {
            None
        };

        let Some((start, end, kind)) = found else {
            i += 1;
            continue;
        };

        let in_word = match prev {
            Before::Word(p)
                if tokens[p].text().eq_ignore_ascii_case("in") && lower != "quote" && de =>
            {
                Some(p)
            }
            _ => None,
        };
        // A determiner before an ambiguous word makes it a noun ("a quote").
        let after_prosody = skip_prosody(tokens, end);
        let vetoed = kind == Kind::Ambiguous
            && ((in_word.is_none() && context::is_prose_use(tokens, start, false))
                || (lower != "quote"
                    && matches!(tokens.get(after_prosody), Some(Token::Word(word))
                        if follows_determiner(&word.to_lowercase()))));
        markers.push(Marker {
            start,
            end,
            kind,
            english: matches!(
                lower.as_str(),
                "quote" | "unquote" | "open" | "begin" | "close" | "end"
            ),
            vetoed,
            in_word,
            role: Role::Keep,
        });
        i = end;
    }

    markers
}

/// Pair openers with closers left to right (see the module docs).
fn resolve_roles(tokens: &[Token], markers: &mut [Marker]) {
    let mut open: Option<usize> = None;

    for k in 0..markers.len() {
        if markers[k].vetoed {
            continue;
        }
        match markers[k].kind {
            Kind::Open => {
                markers[k].role = Role::Open;
                open = Some(k);
            }
            Kind::Close => {
                // A closer without an opener ("a quote unquote …") is a word.
                if open.take().is_some() {
                    markers[k].role = Role::Close;
                }
            }
            Kind::Ambiguous => {
                if let Some(opener) = open {
                    if !markers[k].english && short_pair(tokens, markers[opener], markers[k]) {
                        markers[k].role = Role::Close;
                        open = None;
                    }
                } else if markers.get(k + 1).is_some_and(|next| {
                    !next.vetoed
                        && ((next.kind == Kind::Close && (!markers[k].english || next.english))
                            || (!markers[k].english
                                && next.kind == Kind::Ambiguous
                                && !next.english))
                        && short_pair(tokens, markers[k], *next)
                }) {
                    markers[k].role = Role::Open;
                    open = Some(k);
                } else if markers[k].in_word.is_some()
                    && scare_span(
                        tokens,
                        markers[k].end,
                        markers.get(k + 1).map_or(tokens.len(), |m| m.start),
                    )
                    .is_some()
                {
                    markers[k].role = Role::Scare;
                }
            }
        }
    }
}

/// Ambiguous speech about quotes must not swallow a long stretch of prose.
fn short_pair(tokens: &[Token], opener: Marker, closer: Marker) -> bool {
    let words = tokens[opener.end..closer.start]
        .iter()
        .filter(|token| matches!(token, Token::Word(_)))
        .count();
    (1..=AMBIGUOUS_MAX_WORDS).contains(&words)
}

fn follows_determiner(word: &str) -> bool {
    matches!(
        word,
        "der"
            | "die"
            | "das"
            | "den"
            | "dem"
            | "des"
            | "ein"
            | "eine"
            | "einen"
            | "einem"
            | "einer"
            | "eines"
            | "kein"
            | "keine"
            | "keinen"
            | "keinem"
            | "keiner"
            | "keines"
            | "mein"
            | "meine"
            | "meinen"
            | "meinem"
            | "meiner"
            | "meines"
            | "dein"
            | "deine"
            | "deinen"
            | "deinem"
            | "deiner"
            | "deines"
            | "sein"
            | "seine"
            | "seinen"
            | "seinem"
            | "seiner"
            | "seines"
            | "ihr"
            | "ihre"
            | "ihren"
            | "ihrem"
            | "ihrer"
            | "ihres"
            | "unser"
            | "unsere"
            | "dieser"
            | "diese"
            | "dieses"
            | "diesen"
            | "diesem"
            | "jeder"
            | "jede"
            | "jedes"
            | "welcher"
            | "welche"
            | "welches"
            | "the"
            | "a"
            | "an"
            | "this"
            | "that"
            | "these"
            | "those"
            | "my"
            | "your"
            | "his"
            | "her"
            | "its"
            | "our"
            | "their"
    )
}

/// The token span `(first, end)` quoted by `in Anführungszeichen X`.
fn scare_span(tokens: &[Token], after_marker: usize, limit: usize) -> Option<(usize, usize)> {
    let After::Word(first) = context::after(tokens, after_marker) else {
        return None;
    };
    if first >= limit {
        return None;
    }
    if !context::starts_uppercase(tokens[first].text()) {
        return Some((first, first + 1));
    }
    let mut end = first + 1;
    let mut count = 1;
    while count < SCARE_QUOTE_MAX_WORDS && end + 1 < limit {
        match (tokens.get(end), tokens.get(end + 1)) {
            (Some(Token::Space(space)), Some(Token::Word(word)))
                if !space.contains('\n') && context::starts_uppercase(word) =>
            {
                end += 2;
                count += 1;
            }
            _ => break,
        }
    }
    Some((first, end))
}

fn render(tokens: &[Token], markers: &[Marker]) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut m = 0;
    let mut quoted_start = None;

    while i < tokens.len() {
        // Markers are sorted by position; a dropped "in" starts earlier.
        if let Some(marker) = markers.get(m) {
            let start = match marker.role {
                Role::Open | Role::Scare => marker.in_word.unwrap_or(marker.start),
                _ => marker.start,
            };
            if i == start {
                m += 1;
                let limit = markers.get(m).map_or(tokens.len(), |next| next.start);
                i = emit_marker(tokens, marker, limit, &mut out);
                match marker.role {
                    Role::Open => quoted_start = Some(out.len()),
                    Role::Close => {
                        if let Some(start) = quoted_start.take() {
                            capitalize_sentence(&mut out, start);
                        }
                    }
                    _ => {}
                }
                continue;
            }
        }
        out.push_str(tokens[i].text());
        i += 1;
    }

    out
}

/// Called only for pairs generated from spoken commands. The closing mark
/// has already been emitted, so outside sentence stops cannot affect casing.
fn capitalize_sentence(out: &mut String, start: usize) {
    let Some(close) = out.rfind(QUOTE_MARK).filter(|&end| end >= start) else {
        return;
    };
    let span = &out[start..close];
    if !span.trim_end().ends_with(['.', '!', '?']) {
        return;
    }
    if let Some((offset, first)) = span.char_indices().find(|(_, c)| c.is_alphabetic()) {
        let index = start + offset;
        out.replace_range(
            index..index + first.len_utf8(),
            &first.to_uppercase().to_string(),
        );
    }
}

/// Emit one marker and return the token index to continue from.
fn emit_marker(tokens: &[Token], marker: &Marker, limit: usize, out: &mut String) -> usize {
    match marker.role {
        Role::Keep => {
            let start = marker.start;
            for token in &tokens[start..marker.end] {
                out.push_str(token.text());
            }
            marker.end
        }
        Role::Open => {
            if has_preceding_quotes(tokens, marker.start) {
                absorb_preceding_quotes(out);
            }
            push_opening(out);
            skip_opening_artifacts(tokens, marker.end)
        }
        Role::Scare => {
            push_opening(out);
            let (first, end) =
                scare_span(tokens, marker.end, limit).unwrap_or((marker.end, marker.end));
            for token in &tokens[first..end] {
                out.push_str(token.text());
            }
            out.push(QUOTE_MARK);
            end
        }
        Role::Close => {
            let punctuation_inside = if has_preceding_quotes(tokens, marker.start) {
                absorb_preceding_quotes(out)
            } else {
                None
            };
            let (mut end, following_quote) = absorb_following_quotes(tokens, marker.end);
            trim_end(out);
            let sentence_stop = out.chars().next_back().filter(|c| {
                matches!(c, '.' | '!' | '?' | '…')
                    && (punctuation_inside.is_some() || following_quote)
            });
            let outside = sentence_stop.is_some() && punctuation_inside == Some(false);
            if outside {
                out.pop();
                trim_end(out);
            }
            let preserve_inside_stop = sentence_stop.is_some() && !outside;
            while !preserve_inside_stop && out.ends_with([',', ';', ':', '.', '…']) {
                out.pop();
                trim_end(out);
            }
            out.push(QUOTE_MARK);
            if let Some(stop) = sentence_stop {
                if outside {
                    out.push(stop);
                }
                // The ASR often adds another period to the spoken closer.
                let mut next = end;
                while matches!(tokens.get(next), Some(Token::Space(s)) if !s.contains('\n')) {
                    next += 1;
                }
                if matches!(tokens.get(next), Some(Token::Other(s)) if s.starts_with(stop)) {
                    end = next + 1;
                }
            }
            end
        }
    }
}

fn is_asr_quote(c: char) -> bool {
    matches!(c, '"' | '„' | '“' | '”')
}

fn has_preceding_quotes(tokens: &[Token], start: usize) -> bool {
    tokens[..start]
        .iter()
        .rev()
        .take_while(|token| match token {
            Token::Space(space) => !space.contains('\n'),
            Token::Other(text) => text
                .chars()
                .all(|c| context::is_prosody_char(c) || is_asr_quote(c)),
            Token::Word(_) => false,
        })
        .any(|token| token.text().chars().any(is_asr_quote))
}

/// Absorb adjacent ASR marks, preserving whether their sentence stop was inside.
fn absorb_preceding_quotes(out: &mut String) -> Option<bool> {
    let start = out
        .char_indices()
        .rev()
        .find(|(_, c)| {
            !matches!(c, ' ' | '\t') && !context::is_prosody_char(*c) && !is_asr_quote(*c)
        })
        .map_or(0, |(i, c)| i + c.len_utf8());
    let suffix = &out[start..];
    let quote = suffix.rfind(is_asr_quote)?;
    let stop = suffix.rfind(['.', '!', '?', '…']);
    let inside = stop.is_none_or(|stop| stop < quote);
    let cleaned: String = suffix.chars().filter(|c| !is_asr_quote(*c)).collect();
    out.replace_range(start.., &cleaned);
    Some(inside)
}

/// Skip only spaces belonging to an adjacent quote, retaining prose spacing.
fn absorb_following_quotes(tokens: &[Token], start: usize) -> (usize, bool) {
    let mut i = start;
    let mut end = start;
    loop {
        match tokens.get(i) {
            Some(Token::Space(space)) if !space.contains('\n') => i += 1,
            Some(Token::Other(text)) if text.chars().all(is_asr_quote) => {
                i += 1;
                end = i;
            }
            _ => return (end, end != start),
        }
    }
}

fn skip_opening_artifacts(tokens: &[Token], mut i: usize) -> usize {
    loop {
        let next = skip_prosody(tokens, i);
        if matches!(tokens.get(next), Some(Token::Other(s)) if s.chars().all(is_asr_quote)) {
            i = next + 1;
        } else {
            return next;
        }
    }
}

fn push_opening(out: &mut String) {
    trim_end(out);
    if !out.is_empty() && !out.ends_with(['\n', '(', '[']) {
        out.push(' ');
    }
    out.push(QUOTE_MARK);
}

/// Skip whitespace and prosody punctuation after an opening mark.
fn skip_prosody(tokens: &[Token], mut i: usize) -> usize {
    loop {
        match tokens.get(i) {
            Some(Token::Space(space)) if !space.contains('\n') => i += 1,
            Some(Token::Other(text)) if text.chars().all(context::is_prosody_char) => i += 1,
            _ => return i,
        }
    }
}

fn trim_end(out: &mut String) {
    while out.ends_with([' ', '\t']) {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quotes(text: &str) -> String {
        apply_quotes(text, &[], &[])
    }

    #[test]
    fn explicit_auf_zu() {
        assert_eq!(
            quotes("Er sagte Anführungszeichen auf Hallo Welt Anführungszeichen zu"),
            "Er sagte \"Hallo Welt\""
        );
    }

    #[test]
    fn explicit_with_asr_punctuation() {
        assert_eq!(
            quotes("Er sagte: Anführungszeichen auf, Hallo Welt, Anführungszeichen zu."),
            "Er sagte: \"Hallo Welt\"."
        );
    }

    #[test]
    fn ambiguous_pairing() {
        assert_eq!(
            quotes("Er sagte Anführungszeichen Hallo Anführungszeichen und ging."),
            "Er sagte \"Hallo\" und ging."
        );
    }

    #[test]
    fn user_sample_scare_quotes() {
        assert_eq!(
            quotes("Wir nennen das in Anführungszeichen Feature Freeze."),
            "Wir nennen das \"Feature Freeze\"."
        );
        assert_eq!(
            quotes("Das ist in Anführungszeichen schnell erledigt."),
            "Das ist \"schnell\" erledigt."
        );
    }

    #[test]
    fn english_quote_end_quote() {
        assert_eq!(
            quotes("He said quote this is fine end quote."),
            "He said \"this is fine\"."
        );
        assert_eq!(
            quotes("He said open quote yes close quote, then left."),
            "He said \"yes\", then left."
        );
        assert_eq!(quotes("He said quote no unquote."), "He said \"no\".");
    }

    #[test]
    fn qwen_quotes_around_spoken_commands_are_absorbed() {
        assert_eq!(
            quotes("He said, \"Quote this is fine.\" End quote."),
            "He said, \"This is fine.\""
        );
        for (open, close) in [('"', '"'), ('„', '“'), ('“', '”')] {
            for text in [
                format!("He said, {open}Quote this is fine end quote{close}."),
                format!("He said, Quote{open} this is fine{close} end quote."),
                format!("He said, {open}open quote{open} this is fine{close} close quote{close}."),
                format!("He said, {open}begin quote this is fine{close} unquote."),
            ] {
                assert_eq!(quotes(&text), "He said, \"this is fine\".", "{text}");
            }
        }
    }

    #[test]
    fn german_asr_quotes_around_spoken_commands_are_absorbed() {
        for text in [
            "Er sagte: „Anführungszeichen auf Hallo Welt Anführungszeichen zu“.",
            "Er sagte: Anführungszeichen auf „Hallo Welt“ Anführungszeichen zu.",
            "Er sagte: „Anführungsstriche oben Hallo Welt Anführungsstriche unten“.",
            "Er sagte: „Gänsefüßchen auf Hallo Welt Gänsefüßchen ende“.",
        ] {
            assert_eq!(quotes(text), "Er sagte: \"Hallo Welt\".", "{text}");
        }
    }

    #[test]
    fn asr_sentence_stops_keep_their_side_of_the_closing_quote() {
        for (text, expected) in [
            ("Quote fine.\" End quote.", "\"Fine.\""),
            ("Quote fine\". End quote.", "\"fine\"."),
            ("Quote fine.\" End quote", "\"Fine.\""),
            ("Quote fine\". End quote", "\"fine\"."),
            ("Quote fine\" . End quote.", "\"fine\"."),
            ("Quote fine!\" End quote!", "\"Fine!\""),
            ("Quote fine?\" End quote?", "\"Fine?\""),
            ("Quote fine end quote\".", "\"fine\"."),
            ("Quote fine. end quote.", "\"fine\"."),
            (
                "Quote fine.\" End quote. Next sentence.",
                "\"Fine.\" Next sentence.",
            ),
        ] {
            assert_eq!(quotes(text), expected, "{text}");
        }
    }

    #[test]
    fn existing_asr_quotes_without_commands_are_untouched() {
        for text in [
            "He said, \"this is fine.\"",
            "He said, \"this is fine\".",
            "Er sagte: „Hallo Welt“.",
            "He said, “this is fine.”",
            "Send me \"a quote\" for this job.",
        ] {
            assert_eq!(quotes(text), text, "{text}");
        }
    }

    #[test]
    fn consecutive_spoken_pairs_keep_generated_closing_marks() {
        assert_eq!(
            quotes("open quote yes close quote open quote no close quote"),
            "\"yes\" \"no\""
        );
    }

    #[test]
    fn lone_or_noun_quote_words_stay_words() {
        for text in [
            "Er sagte Anführungszeichen sind wichtig.",
            "Die Anführungszeichen fehlen.",
            "I quote the article.",
            "Send me a quote for the quote module.",
            "It is a quote unquote feature.",
        ] {
            assert_eq!(quotes(text), text);
        }
    }

    #[test]
    fn disabled_language_key() {
        let disabled = vec![KEY_DE.to_string()];
        let text = "Er sagte Anführungszeichen auf Hallo Anführungszeichen zu";
        assert_eq!(apply_quotes(text, &[], &disabled), text);
    }

    #[test]
    fn bare_quotes_require_unambiguous_short_pairs() {
        for text in [
            "Please quote the source and quote the page number.",
            "Please quote hello quote goodbye.",
            "Anführungszeichen die Quelle und Anführungszeichen die Seitenzahl.",
            "Anführungszeichen, die Quelle und Anführungszeichen, die Seitenzahl.",
            "quote hello Anführungszeichen zu",
            "Anführungszeichen eins zwei drei vier fünf sechs sieben acht neun Anführungszeichen",
            "quote one two three four five six seven eight nine end quote",
        ] {
            assert_eq!(quotes(text), text, "input: {text}");
        }
        assert_eq!(
            quotes("quote one two three four five six seven eight close quote"),
            "\"one two three four five six seven eight\""
        );
        assert_eq!(
            quotes(
                "Anführungszeichen eins zwei drei vier fünf sechs sieben acht Anführungszeichen"
            ),
            "\"eins zwei drei vier fünf sechs sieben acht\""
        );
    }

    #[test]
    fn scare_quotes_stop_before_next_marker() {
        assert_eq!(
            quotes("in Anführungszeichen Hallo Anführungszeichen auf Welt Anführungszeichen zu"),
            "\"Hallo\" \"Welt\""
        );
        assert_eq!(
            quotes("in Anführungszeichen Feature Freeze open quote next close quote"),
            "\"Feature Freeze\" \"next\""
        );
        assert_eq!(
            quotes("in Anführungszeichen Hallo Anführungszeichen die Quelle"),
            "\"Hallo\" Anführungszeichen die Quelle"
        );
    }
    #[test]
    fn generated_sentence_quotes_capitalize_the_first_letter_only() {
        for (input, expected) in [
            (
                "He said, \"Quote this is fine.\" End quote.",
                "He said, \"This is fine.\"",
            ),
            ("open quote yes! close quote", "\"Yes!\""),
            ("open quote why? close quote", "\"Why?\""),
            ("open quote übermorgen! close quote", "\"Übermorgen!\""),
            ("open quote 42 answers! close quote", "\"42 Answers!\""),
            ("open quote iPhone works! close quote", "\"IPhone works!\""),
            ("open quote iPhone close quote.", "\"iPhone\"."),
            ("open quote this is fine close quote.", "\"this is fine\"."),
            ("open quote THIS close quote.", "\"THIS\"."),
            (
                "open quote yes! close quote open quote no close quote",
                "\"Yes!\" \"no\"",
            ),
        ] {
            assert_eq!(quotes(input), expected, "{input}");
            assert_eq!(quotes(&quotes(input)), expected, "idempotence: {input}");
        }
    }
}
