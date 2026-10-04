//! Disfluency detection: does the transcript hold something a minimal-edit
//! cleanup should remove — a hesitation sound, a stutter, an aborted start or
//! a self-repair? Each finding is a [`Span`]; the LLM pass runs only when there
//! is one, and the over-edit guard allows deletions only at these spans.
//!
//! The pass runs on the final text, i.e. **after** upstream's deterministic
//! filler removal (`remove_filler_words`, on by default: universal `uh`, `hmm`,
//! `ehm`, … plus language-gated `äh`/`ähm` for German, `um`/`ah`/`eh` for
//! English) and stutter collapse (3+ repeats). Whatever upstream already
//! removed needs no LLM. Kinds:
//! - `filler`: a hesitation sound upstream left in (language unknown, removal
//!   disabled, or not on its lists: `öhm`, `mhm`, `erm`). `er`/`erm`/`um` are
//!   German words, so they count only for English.
//! - `repetition`: an immediate word or two-word repeat without a pause
//!   between ("ich ich", "the the", "ich bin ich bin") — except legitimate
//!   doubles ("die die" relative clauses, "Sie sie", "had had", "sehr sehr").
//! - `restart`: a dash or ellipsis (or a surviving filler) whose next words
//!   restate the words before it ([`restates`]) or restart a fragment of at
//!   most two (filler: three) words with one of its words ("Ich wollte – ich
//!   muss los"); or — since upstream's filler removal leaves
//!   "morgen, äh, übermorgen" as "morgen, übermorgen" and the pre-filler text
//!   is not passed here — a comma between two restating words of the same
//!   weekday / month / relative-day class, or a repeated frame ("am Montag,
//!   am Dienstag"). Number and name slots need the frame there ("drei, vier
//!   Tage" is an estimate, "Danke Anna, Paul …" an address); lists ("…, und",
//!   a third item) never count.
//!   Adjacent weekday/month/relative-day slots also count without punctuation;
//!   adjacent numbers or names do not.
//! - `cue:<words>`: an explicit correction cue ([`find_cues`]).

use super::cues::{
    clause_after, clause_before, find_cues, gap, restates, slot_kind, tokenize, word_strs,
    AnchorKind, Token, ALL_SLOTS,
};
use std::ops::Range;

/// Hesitation sounds in any language.
const FILLERS: &[&str] = &[
    "äh", "ähm", "ähh", "öh", "öhm", "hm", "hmm", "hmmm", "mhm", "uh", "uhm", "uhh", "umm", "ehm",
    "ahm", "ehh", "mmm",
];
/// Hesitation sounds that are ordinary German words ("er kommt um drei").
const ENGLISH_FILLERS: &[&str] = &["um", "er", "erm"];

/// Doubles that are grammar or emphasis, not a stutter.
const LEGIT_DOUBLES: &[&str] = &[
    "das", "die", "der", "den", "dem", "des", "sie", "ja", "nein", "nee", "sehr", "ganz", "viel",
    "viele", "immer", "mal", "gut", "na", "so", "oh", "ah", "ha", "haha", "hey", "hallo", "bitte",
    "danke", "komm", "schnell", "lange", "mehr", "that", "had", "is", "very", "really", "much",
    "many", "far", "long", "more", "yes", "no", "bye", "too", "now", "come", "please", "okay",
    "ok",
];

/// Words that continue a list or range — a comma before them is not a repair.
const CONJUNCTIONS: &[&str] = &[
    "und", "oder", "sowie", "bis", "bzw", "and", "or", "to", "through", "until",
];

/// The longest fragment a dash / filler restart may abandon.
const MAX_DASH_FRAGMENT: usize = 2;
const MAX_FILLER_FRAGMENT: usize = 3;

/// One detected disfluency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Span {
    /// Journal kind: `filler`, `repetition`, `restart`, `cue:<words>`.
    pub(super) label: String,
    /// Word positions the cleanup may delete outright (the filler, the
    /// repeated words, the cue); empty for a restart at a comma or dash.
    pub(super) words: Range<usize>,
    /// For a self-repair: the earliest word position of the retracted part,
    /// which must end within two words of `words`.
    pub(super) retract_from: Option<usize>,
}

fn is_filler(word: &str, english: bool) -> bool {
    FILLERS.contains(&word) || (english && ENGLISH_FILLERS.contains(&word))
}

/// Every disfluency in `text`. `lang` is the transcription language (`en`,
/// `de-DE`, …), if known.
pub(super) fn detect(text: &str, lang: Option<&str>) -> Vec<Span> {
    let english = lang.is_some_and(|l| l.split(['-', '_']).next() == Some("en"));
    let tokens = tokenize(text);
    // Word position of every token (a pause gets the next word's position).
    let mut position = Vec::with_capacity(tokens.len() + 1);
    let mut n = 0;
    for token in &tokens {
        position.push(n);
        if matches!(token, Token::Word(..)) {
            n += 1;
        }
    }
    position.push(n);
    let word = |i: usize| match tokens.get(i) {
        Some(Token::Word(w, _)) => Some(w.as_str()),
        _ => None,
    };

    let mut spans: Vec<Span> = find_cues(text)
        .into_iter()
        .map(|(cue, words, from)| Span {
            label: format!("cue:{cue}"),
            words,
            retract_from: Some(from),
        })
        .collect();

    for i in 0..tokens.len() {
        let Some(w) = word(i) else {
            // A dash / ellipsis between two words: an aborted start?
            let g = gap(text, &tokens, i);
            if g.contains(['–', '—', '…']) || g.contains("...") || g.contains(" - ") {
                if let Some(from) = restart(&tokens, i, i + 1, MAX_DASH_FRAGMENT) {
                    spans.push(restart_span(position[i + 1], position[from]));
                }
            } else if g.starts_with(',') && g.len() > 1 && g[1..].chars().all(char::is_whitespace) {
                if let Some(from) = comma_restatement(text, &tokens, i) {
                    spans.push(restart_span(position[i + 1], position[from]));
                }
            }
            continue;
        };
        // No punctuation survives when Qwen drops the hesitation entirely.
        if let Some(next) = word(i + 1) {
            let whitespace_only = match (&tokens[i], &tokens[i + 1]) {
                (Token::Word(_, before), Token::Word(_, after)) => text[*before..*after]
                    .trim_end()
                    .chars()
                    .all(char::is_alphanumeric),
                _ => false,
            };
            if w != next
                && whitespace_only
                && slot_kind(w).is_some_and(|kind| {
                    matches!(
                        kind,
                        AnchorKind::RelativeDay | AnchorKind::Weekday | AnchorKind::Month
                    ) && slot_kind(next) == Some(kind)
                })
                && !(w == "morgen" && i > 0 && word(i - 1) == Some("guten"))
            {
                spans.push(restart_span(position[i + 1], position[i]));
            }
        }
        if is_filler(w, english) {
            let before = if i > 0 && tokens[i - 1] == Token::Pause {
                i - 1
            } else {
                i
            };
            let after = if tokens.get(i + 1) == Some(&Token::Pause) {
                i + 2
            } else {
                i + 1
            };
            spans.push(Span {
                label: "filler".into(),
                words: position[i]..position[i] + 1,
                retract_from: restart(&tokens, before, after, MAX_FILLER_FRAGMENT)
                    .map(|from| position[from]),
            });
            continue;
        }
        if !w.chars().all(char::is_alphabetic) || LEGIT_DOUBLES.contains(&w) {
            continue;
        }
        if word(i + 1) == Some(w) {
            spans.push(Span {
                label: "repetition".into(),
                words: position[i]..position[i] + 2,
                retract_from: None,
            });
        } else if word(i + 1).is_some_and(|b| !is_filler(b, english))
            && word(i + 2) == Some(w)
            && word(i + 3) == word(i + 1)
        {
            spans.push(Span {
                label: "repetition".into(),
                words: position[i]..position[i] + 4,
                retract_from: None,
            });
        }
    }
    spans
}

fn restart_span(at: usize, from: usize) -> Span {
    Span {
        label: "restart".into(),
        words: at..at,
        retract_from: Some(from),
    }
}

/// A restart across the boundary tokens `end..start` (a filler or a dash):
/// the clause after restates the clause before, or restarts a short fragment
/// with one of its words. Returns the token where the retracted part starts.
fn restart(tokens: &[Token], end: usize, start: usize, max_fragment: usize) -> Option<usize> {
    if start >= tokens.len() {
        return None;
    }
    let left_range = clause_before(tokens, end);
    let left = word_strs(tokens, left_range.clone());
    let right = word_strs(tokens, clause_after(tokens, start));
    if let Some(from) = restates(&left, &right, ALL_SLOTS) {
        return Some(left_range.start + from);
    }
    let first = right.first()?;
    (left.len() <= max_fragment && left.contains(first)).then_some(left_range.start)
}

/// "morgen, übermorgen": the clause after the comma at token `pause` restates
/// the clause before it, outside a list. Returns the retracted part's token.
fn comma_restatement(text: &str, tokens: &[Token], pause: usize) -> Option<usize> {
    const FRAMELESS: &[AnchorKind] = &[
        AnchorKind::Weekday,
        AnchorKind::Month,
        AnchorKind::RelativeDay,
    ];
    let left_range = clause_before(tokens, pause);
    let right_range = clause_after(tokens, pause + 1);
    let left = word_strs(tokens, left_range.clone());
    let right = word_strs(tokens, right_range.clone());
    let from = restates(&left, &right, FRAMELESS)?;
    // A conjunction right after the replacement continues a list or range.
    if right.iter().take(4).any(|w| CONJUNCTIONS.contains(w)) {
        return None;
    }
    // A third comma-separated item before or after: a list.
    let list_neighbour = |pause: usize, side: usize| {
        tokens.get(pause) == Some(&Token::Pause)
            && gap(text, tokens, pause).starts_with(',')
            && matches!(tokens.get(side), Some(Token::Word(..)))
    };
    let next = right_range.end;
    let previous = left_range.start.checked_sub(1);
    let neighbour_restates = |l: Vec<&str>, r: Vec<&str>| restates(&l, &r, ALL_SLOTS).is_some();
    if list_neighbour(next, next + 1)
        && neighbour_restates(
            right.clone(),
            word_strs(tokens, clause_after(tokens, next + 1)),
        )
    {
        return None;
    }
    if let Some(previous) = previous {
        if previous >= 1
            && list_neighbour(previous, previous - 1)
            && neighbour_restates(word_strs(tokens, clause_before(tokens, previous)), left)
        {
            return None;
        }
    }
    Some(left_range.start + from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(text: &str, lang: &str) -> Vec<String> {
        detect(text, Some(lang))
            .into_iter()
            .map(|span| span.label)
            .collect()
    }

    #[test]
    fn detects_disfluencies() {
        for (text, lang, label) in [
            // Fillers upstream left in (language unknown or not on its list).
            ("Wir brauchen, ähm, drei Tickets.", "de", "filler"),
            ("Öhm, ich glaube schon.", "de", "filler"),
            ("We need, erm, three tickets.", "en", "filler"),
            ("We need, um, three tickets.", "en", "filler"),
            ("Mhm, das passt.", "de", "filler"),
            // Stutters.
            ("Das ist ist gut.", "de", "repetition"),
            ("Ich ich komme gleich.", "de", "repetition"),
            ("Put it on the the table.", "en", "repetition"),
            ("Ich bin ich bin gleich da.", "de", "repetition"),
            // Aborted starts and restatements.
            ("Ich wollte – ich muss jetzt los.", "de", "restart"),
            ("Ich wollte... ich muss jetzt los.", "de", "restart"),
            ("Ich komme morgen, übermorgen.", "de", "restart"),
            ("Wir treffen uns am Montag, am Dienstag.", "de", "restart"),
            ("Treffen um drei, um vier.", "de", "restart"),
            ("Nimm den roten Stift, den blauen Stift.", "de", "restart"),
            // Cues without a number anchor.
            ("Schick das an Tom, nein, an Tim.", "de", "cue:nein"),
            ("Call Anna, no, call Mark.", "en", "cue:no"),
            (
                "Nimm den roten, nein warte, den blauen.",
                "de",
                "cue:nein warte",
            ),
            ("Bring Wein, Korrektur, bring Bier.", "de", "cue:korrektur"),
            ("Ich nehme Tee, ich meinte, Kaffee.", "de", "cue:ich meinte"),
            (
                "Use the red folder, I mean, the blue folder.",
                "en",
                "cue:i mean",
            ),
        ] {
            assert!(
                labels(text, lang).iter().any(|l| l == label),
                "{text}: {:?}",
                labels(text, lang)
            );
        }
    }

    #[test]
    fn ordinary_prose_does_not_trigger() {
        for (text, lang) in [
            ("Er hat um drei Uhr angerufen.", "de"),
            ("Er kommt um vier, er bleibt bis sechs.", "de"),
            ("Die Leute, die die Regeln kennen, wissen das.", "de"),
            ("Haben Sie sie gesehen?", "de"),
            ("Das ist sehr sehr gut.", "de"),
            ("Ja ja, schon gut.", "de"),
            ("He said that that was fine.", "en"),
            ("She had had enough.", "en"),
            ("Montag, Dienstag und Mittwoch passen.", "de"),
            ("Montag, Dienstag, Mittwoch passen.", "de"),
            ("Am Montag, am Dienstag und am Mittwoch.", "de"),
            ("Das dauert drei, vier Tage.", "de"),
            ("Danke Anna, Paul kommt auch.", "de"),
            ("Guten Morgen, heute ist Markttag.", "de"),
            ("Ich glaube, ich komme später.", "de"),
            ("Ist es Montag? Nein, Dienstag.", "de"),
            ("Ich weiß nicht... ich glaube schon.", "de"),
            ("Der Wert ist 3,5 Prozent.", "de"),
            ("Treffen um 15:30 Uhr.", "de"),
            ("Ich meine, das ist eine gute Idee.", "de"),
            ("Er sagte um drei Uhr nein.", "de"),
            ("", "de"),
        ] {
            assert_eq!(labels(text, lang), Vec::<String>::new(), "{text}");
        }
    }

    #[test]
    fn english_only_fillers_need_english() {
        assert!(labels("Er, um, drei.", "de").is_empty());
        assert!(detect("We need, um, three.", None).is_empty());
        assert_eq!(labels("We need, um, three.", "en-US"), vec!["filler"]);
    }

    #[test]
    fn spans_mark_deletable_words_and_retractions() {
        let spans = detect("Ich komme morgen, äh, übermorgen.", Some("de"));
        assert_eq!(
            spans,
            vec![Span {
                label: "filler".into(),
                words: 3..4,
                retract_from: Some(2),
            }]
        );
        let spans = detect("Ich komme morgen, übermorgen.", Some("de"));
        assert_eq!(spans, vec![restart_span(3, 2)]);
        let spans = detect("Wir brauchen, ähm, drei Tickets.", Some("de"));
        assert_eq!(spans[0].retract_from, None);
        let spans = detect("Das ist ist gut.", Some("de"));
        assert_eq!(spans[0].words, 1..3);
    }
    #[test]
    fn qwen_repairs_without_fillers_or_original_punctuation() {
        for (text, label) in [
            ("Ich komme morgen übermorgen.", "restart"),
            ("Schick das an Tom. Nein, an Tim.", "cue:nein"),
            ("Schick das an Tom. Nein an Tim.", "cue:nein"),
            ("Schick das an Tom. Nee an Tim.", "cue:nee"),
            ("Send it to Tom. No to Tim.", "cue:no"),
            (
                "Wir treffen uns um drei und dann warte um vier.",
                "cue:warte",
            ),
            ("Montag Dienstag passt.", "restart"),
            ("Januar Februar passt.", "restart"),
            ("Come today tomorrow.", "restart"),
            ("Meet March May.", "restart"),
        ] {
            assert!(labels(text, "de").iter().any(|l| l == label), "{text}");
        }
        for (text, span) in [
            ("Ich komme morgen übermorgen.", restart_span(3, 2)),
            (
                "Schick das an Tom. Nein, an Tim.",
                Span {
                    label: "cue:nein".into(),
                    words: 4..5,
                    retract_from: Some(2),
                },
            ),
            (
                "Schick das an Tom. Nein an Tim.",
                Span {
                    label: "cue:nein".into(),
                    words: 4..5,
                    retract_from: Some(2),
                },
            ),
            (
                "Wir treffen uns um drei und dann warte um vier.",
                Span {
                    label: "cue:warte".into(),
                    words: 7..8,
                    retract_from: Some(3),
                },
            ),
        ] {
            assert_eq!(detect(text, Some("de")), vec![span], "{text}");
        }
    }

    #[test]
    fn qwen_rule_negatives_stay_ordinary_prose() {
        for text in [
            "Das dauert drei vier Tage.",
            "Anna Maria kommt.",
            "Ist es Montag? Nein, Dienstag.",
            "Ist es Montag? Nein Dienstag.",
            "Das war gut. Nein, wirklich.",
            "Das war gut. Nein wirklich.",
            "Nein danke.",
            "Dann warte ich um vier auf dich.",
            "Warte auf mich um drei.",
            "Ich warte seit drei Uhr, um vier gehe ich.",
            "Guten Morgen heute ist Markttag.",
            "Ich komme morgen/übermorgen.",
        ] {
            assert!(detect(text, Some("de")).is_empty(), "{text}");
        }
    }
    #[test]
    fn detects_generalized_explicit_repairs_but_preserves_normal_speech() {
        for (text, cue) in [
            ("Ich komme morgen. Ne, übermorgen.", "ne"),
            ("Ich komme morgen. Korrigiere übermorgen.", "korrigiere"),
            (
                "Treffen am Montag, beziehungsweise Dienstag.",
                "beziehungsweise",
            ),
            ("Ruf Anna an. Sorry, ruf Lena an.", "sorry"),
            ("See you at five, make that six.", "make that"),
            ("Wir brauchen drei, korrigiere, vier Tickets.", "korrigiere"),
        ] {
            assert!(labels(text, "de").contains(&format!("cue:{cue}")), "{text}");
        }
        for text in [
            "Ne, das passt schon.",
            "Nein danke.",
            "Das ist gut. Nein, wirklich.",
            "Ist es Montag? Nein, Dienstag.",
            "Ich nehme Tee bzw. Kaffee.",
            "Sorry, ich bin spät dran.",
            "Korrigiere bitte den Text.",
            "Besser gesagt ist das nicht.",
            "Rather than waiting, we go now.",
        ] {
            assert!(detect(text, Some("de")).is_empty(), "{text}");
        }
    }
}
