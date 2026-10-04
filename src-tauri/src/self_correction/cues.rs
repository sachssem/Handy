//! Self-correction cue detection: does the transcript contain a spoken
//! "no wait / I mean / scratch that" that retracts what came before?
//!
//! Every cue needs preceding dictation and, except a bounded inline wait,
//! a pause directly before it (including a new clause after a period). Words
//! compare case-insensitively; pauses between cue words are skipped. Questions
//! before or inside a cue and reported speech never license a repair.
//!
//! Bare negations and ambiguous phrases/apologies need an immediate
//! restatement: a repeated frame or a first replacement word sharing the class
//! of a slot in the preceding clause. Explicit correction verbs/phrases also
//! count when delimited by pauses on both sides. `ich meine` / `I mean` keep
//! their existing rule: pauses on both sides and an anchor within three words
//! or a restatement. Bare clause-initial and bounded inline waits keep their
//! local matching-anchor rules. No cue counts at the start of dictation.

use std::ops::Range;

/// When a cue counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Pause mark before.
    Strong,
    /// Pause mark before and after.
    Delimited,
    /// Pause mark before and after, and a number / name within 3 words.
    Weak,
    /// Clause-initial wait, or bounded inline wait with matching local slots.
    AnchoredWait,
    /// A restatement, or pause marks before and after an explicit correction.
    Repair,
    /// An immediate restatement; required for short or ambiguous cues.
    Restatement,
}

/// (cue words, kind). Single source for the docs in `docs/fork-patches.md`.
const CUES: &[(&[&str], Kind)] = &[
    // German
    (&["nein", "warte"], Kind::Strong),
    (&["nee", "warte"], Kind::Strong),
    (&["nein", "moment"], Kind::Strong),
    (&["nein", "ich", "meine"], Kind::Strong),
    (&["nein", "ich", "meinte"], Kind::Strong),
    (&["sorry", "ich", "meine"], Kind::Strong),
    (&["sorry", "ich", "meinte"], Kind::Strong),
    (&["ich", "korrigiere"], Kind::Repair),
    (&["korrigiere"], Kind::Repair),
    (&["korrigier"], Kind::Repair),
    (&["korrektur"], Kind::Repair),
    (&["berichtige"], Kind::Repair),
    (&["besser", "gesagt"], Kind::Repair),
    (&["genauer", "gesagt"], Kind::Repair),
    (&["oder", "besser"], Kind::Repair),
    (&["oder", "vielmehr"], Kind::Repair),
    (&["vielmehr"], Kind::Restatement),
    (&["beziehungsweise"], Kind::Restatement),
    (&["bzw"], Kind::Restatement),
    (&["entschuldigung"], Kind::Restatement),
    (&["pardon"], Kind::Restatement),
    (&["streich", "das"], Kind::Delimited),
    (&["streiche", "das"], Kind::Delimited),
    (&["vergiss", "das"], Kind::Delimited),
    (&["ich", "meinte"], Kind::Delimited),
    (&["ich", "meine"], Kind::Weak),
    (&["warte"], Kind::AnchoredWait),
    (&["moment"], Kind::AnchoredWait),
    (&["nein"], Kind::Restatement),
    (&["nee"], Kind::Restatement),
    (&["ne"], Kind::Restatement),
    (&["nö"], Kind::Restatement),
    // English
    (&["no", "wait"], Kind::Strong),
    (&["no", "i", "mean"], Kind::Strong),
    (&["no", "i", "meant"], Kind::Strong),
    (&["sorry", "i", "mean"], Kind::Strong),
    (&["sorry", "i", "meant"], Kind::Strong),
    (&["wait", "no"], Kind::Delimited),
    (&["actually", "no"], Kind::Delimited),
    (&["scratch", "that"], Kind::Delimited),
    (&["strike", "that"], Kind::Delimited),
    (&["correction"], Kind::Repair),
    (&["or", "rather"], Kind::Restatement),
    (&["rather"], Kind::Restatement),
    (&["make", "that"], Kind::Repair),
    (&["let", "me", "rephrase"], Kind::Repair),
    (&["i", "meant"], Kind::Delimited),
    (&["i", "mean"], Kind::Weak),
    (&["wait"], Kind::AnchoredWait),
    (&["no"], Kind::Restatement),
    (&["nope"], Kind::Restatement),
    (&["nah"], Kind::Restatement),
    (&["sorry"], Kind::Restatement),
];

/// Number words, weekdays and months that make a weak cue count.
const NUMBER_WORDS: &[&str] = &[
    "zwei", "drei", "vier", "fünf", "sechs", "sieben", "acht", "neun", "zehn", "elf", "zwölf",
    "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten", "eleven", "twelve",
];
const WEEKDAYS: &[&str] = &[
    "montag",
    "dienstag",
    "mittwoch",
    "donnerstag",
    "freitag",
    "samstag",
    "sonntag",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
    "sunday",
];
const MONTHS: &[&str] = &[
    "januar",
    "februar",
    "märz",
    "april",
    "mai",
    "juni",
    "juli",
    "august",
    "september",
    "oktober",
    "november",
    "dezember",
    "january",
    "february",
    "june",
    "july",
    "october",
    "december",
];

/// Relative days: a slot of their own for restatements ("morgen, übermorgen"),
/// but too ambiguous to anchor a weak cue on their own.
const RELATIVE_DAYS: &[&str] = &[
    "heute",
    "morgen",
    "übermorgen",
    "gestern",
    "vorgestern",
    "today",
    "tomorrow",
    "yesterday",
    "tonight",
];

/// Frequent words of four or more letters that never make a repeated "head
/// noun" (a restatement repeats content, not grammar).
const STOP_WORDS: &[&str] = &[
    "aber", "auch", "dann", "doch", "noch", "eine", "einen", "einem", "einer", "eines", "dass",
    "nicht", "habe", "haben", "hast", "sind", "wird", "werden", "kann", "muss", "soll", "will",
    "mein", "meine", "dein", "deine", "sein", "seine", "ihre", "unser", "this", "that", "with",
    "have", "from", "your", "they", "them", "were", "will", "would", "could", "should", "what",
    "there", "their", "about",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Token {
    Word(String, usize),
    Pause,
}

fn is_pause_mark(c: char) -> bool {
    matches!(
        c,
        ',' | '.' | ';' | ':' | '!' | '?' | '…' | '–' | '—' | '\n'
    )
}

pub(super) fn tokenize(text: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut word = String::new();
    let mut word_start = 0;
    let flush = |word: &mut String, start: usize, tokens: &mut Vec<Token>| {
        if !word.is_empty() {
            tokens.push(Token::Word(word.to_lowercase(), start));
            word.clear();
        }
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (i, &(offset, c)) in chars.iter().enumerate() {
        // Keep apostrophes inside words ("I'm"), split on everything else.
        if c.is_alphanumeric()
            || (matches!(c, '\'' | '’')
                && !word.is_empty()
                && chars.get(i + 1).is_some_and(|(_, n)| n.is_alphanumeric()))
        {
            if word.is_empty() {
                word_start = offset;
            }
            word.push(c);
            continue;
        }
        flush(&mut word, word_start, &mut tokens);
        // A spaced hyphen is a dash pause ("drei - nein warte").
        let spaced_hyphen = c == '-'
            && chars
                .get(i.wrapping_sub(1))
                .is_some_and(|(_, p)| p.is_whitespace())
            && chars.get(i + 1).is_some_and(|(_, n)| n.is_whitespace());
        if (is_pause_mark(c) || spaced_hyphen) && tokens.last() != Some(&Token::Pause) {
            tokens.push(Token::Pause);
        }
    }
    flush(&mut word, word_start, &mut tokens);
    tokens
}

/// Whether a word anchors a weak cue: a number, weekday, month or first name.
fn is_anchor(word: &str) -> bool {
    anchor_kind(word).is_some()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AnchorKind {
    Number,
    Weekday,
    Month,
    Name,
    RelativeDay,
}

/// Every slot kind: restatements after an explicit cue, filler or dash.
pub(super) const ALL_SLOTS: &[AnchorKind] = &[
    AnchorKind::Number,
    AnchorKind::Weekday,
    AnchorKind::Month,
    AnchorKind::Name,
    AnchorKind::RelativeDay,
];

/// The word class a restatement must keep: an anchor kind or a relative day.
pub(super) fn slot_kind(word: &str) -> Option<AnchorKind> {
    if matches!(word, "march" | "may") {
        Some(AnchorKind::Month)
    } else if RELATIVE_DAYS.contains(&word) {
        Some(AnchorKind::RelativeDay)
    } else {
        anchor_kind(word)
    }
}

/// Whether the clause words `right` (after a boundary) restate the clause
/// words `left` (before it) — the replacement of a self-repair. Returns the
/// index in `left` where the retracted part starts.
///
/// - Slot: the first replacement word shares a class allowed by `frameless`
///   with a word in the preceding clause (the nearest matching slot wins).
/// - Frame: the first replacement word repeats one of the last three clause
///   words, followed within two words by a matching slot or repeated head noun.
pub(super) fn restates(left: &[&str], right: &[&str], frameless: &[AnchorKind]) -> Option<usize> {
    let (&last, &first) = (left.last()?, right.first()?);
    let kind_at = |i: usize| {
        slot_kind(left[i]).filter(|_| {
            // “Guten Morgen” greets; it retracts nothing.
            !(left[i] == "morgen" && i > 0 && left[i - 1] == "guten")
        })
    };
    let head_noun = last.chars().count() >= 4 && !STOP_WORDS.contains(&last);
    let tail = left.len().saturating_sub(3);
    // A repeated frame may precede an internal slot (“ruf Anna an” →
    // “ruf Lena an”), not just the final word of the clause.
    for frame in (tail..left.len().saturating_sub(1)).rev() {
        if left[frame] == first
            && right[1..].iter().take(2).any(|&w| {
                (head_noun && w == last)
                    || (frame + 1..left.len())
                        .take(2)
                        .any(|i| kind_at(i).is_some_and(|k| slot_kind(w) == Some(k)))
            })
        {
            return Some(frame);
        }
    }
    let replacement_kind = slot_kind(first)?;
    if !frameless.contains(&replacement_kind) {
        return None;
    }
    // The cue's first replacement word can repair a slot anywhere in the
    // preceding clause (“at five tonight, make that six”).
    (0..left.len())
        .rev()
        .find(|&i| left[i] != first && kind_at(i) == Some(replacement_kind))
}

/// The words of the clause that ends right before token `end` (back to the
/// previous pause), as token indices.
pub(super) fn clause_before(tokens: &[Token], end: usize) -> Range<usize> {
    let start = tokens[..end]
        .iter()
        .rposition(|t| *t == Token::Pause)
        .map_or(0, |i| i + 1);
    start..end
}

/// The words of the clause that starts at token `start` (up to the next
/// pause), as token indices.
pub(super) fn clause_after(tokens: &[Token], start: usize) -> Range<usize> {
    let end = tokens[start..]
        .iter()
        .position(|t| *t == Token::Pause)
        .map_or(tokens.len(), |i| start + i);
    start..end
}

pub(super) fn word_strs(tokens: &[Token], range: Range<usize>) -> Vec<&str> {
    tokens[range]
        .iter()
        .filter_map(|t| match t {
            Token::Word(w, _) => Some(w.as_str()),
            Token::Pause => None,
        })
        .collect()
}

/// The raw text between the words around the pause token `pause` (the
/// punctuation and spaces the ASR put there), or "" without a word on both
/// sides.
pub(super) fn gap<'a>(text: &'a str, tokens: &[Token], pause: usize) -> &'a str {
    let (Some(Token::Word(_, before)), Some(Token::Word(_, after))) = (
        pause.checked_sub(1).and_then(|i| tokens.get(i)),
        tokens.get(pause + 1),
    ) else {
        return "";
    };
    let word = &text[*before..*after];
    let end = word
        .find(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '’')))
        .unwrap_or(word.len());
    &word[end..]
}

fn anchor_kind(word: &str) -> Option<AnchorKind> {
    // Names such as May must not reintroduce ambiguous ordinary words.
    if ["one", "may", "march", "today", "heute", "morgen"].contains(&word) {
        return None;
    }
    if word.chars().any(|c| c.is_ascii_digit()) || NUMBER_WORDS.contains(&word) {
        Some(AnchorKind::Number)
    } else if WEEKDAYS.contains(&word) {
        Some(AnchorKind::Weekday)
    } else if MONTHS.contains(&word) {
        Some(AnchorKind::Month)
    } else if crate::correction_learning::lexicon::is_name(word) {
        Some(AnchorKind::Name)
    } else {
        None
    }
}

fn matching_wait_anchors(tokens: &[Token], start: usize, end: usize) -> bool {
    // Only the immediately preceding clause can supply the retracted anchor.
    let previous = &tokens[..start - 1];
    let clause_start = previous
        .iter()
        .rposition(|t| *t == Token::Pause)
        .map_or(0, |i| i + 1);
    let after = if tokens.get(end) == Some(&Token::Pause) {
        end + 1
    } else {
        end
    };
    tokens[after..]
        .iter()
        .take_while(|t| **t != Token::Pause)
        .filter_map(|t| match t {
            Token::Word(w, _) => Some(w),
            Token::Pause => None,
        })
        .take(3)
        .filter_map(|w| anchor_kind(w))
        .any(|kind| {
            previous[clause_start..]
                .iter()
                .any(|t| matches!(t, Token::Word(w, _) if anchor_kind(w) == Some(kind)))
        })
}

/// Whether the clause after the cue at `start..end` restates the clause
/// before the pause mark preceding it.
fn restates_around(tokens: &[Token], start: usize, end: usize) -> bool {
    restatement_from(tokens, start, end).is_some()
}

fn restatement_from(tokens: &[Token], start: usize, end: usize) -> Option<usize> {
    let after = if tokens.get(end) == Some(&Token::Pause) {
        end + 1
    } else {
        end
    };
    let before_end = if tokens.get(start.wrapping_sub(1)) == Some(&Token::Pause) {
        start - 1
    } else {
        start
    };
    let before = clause_before(tokens, before_end);
    let left = word_strs(tokens, before.clone());
    let right = word_strs(tokens, clause_after(tokens, after));
    restates(&left, &right, ALL_SLOTS).map(|from| before.start + from)
}

/// ASR can turn “nein warte” into “und dann warte”, without punctuation.
/// Match a replacement in the next two words against a slot in the previous
/// six words of this sentence; names do not anchor an inline wait.
fn inline_wait_retraction(text: &str, tokens: &[Token], start: usize, end: usize) -> Option<usize> {
    let after = if tokens.get(end) == Some(&Token::Pause) {
        if gap(text, tokens, end).contains(['.', '!', '?']) {
            return None;
        }
        end + 1
    } else {
        end
    };
    let right = word_strs(tokens, clause_after(tokens, after));
    // A pronoun introduces an ordinary finite-verb clause, not a repair.
    if right.first().is_some_and(|w| {
        [
            "ich", "du", "er", "sie", "es", "wir", "ihr", "i", "you", "he", "she", "it", "we",
            "they",
        ]
        .contains(w)
    }) {
        return None;
    }
    let replacement = right
        .iter()
        .take(2)
        .find_map(|w| slot_kind(w).filter(|kind| *kind != AnchorKind::Name))?;
    let mut seen = 0;
    for i in (0..start).rev() {
        match &tokens[i] {
            Token::Pause if gap(text, tokens, i).contains(['.', '!', '?', ':']) => break,
            Token::Pause => continue,
            Token::Word(w, _) => {
                seen += 1;
                if seen > 6 {
                    break;
                }
                if slot_kind(w) == Some(replacement) {
                    // A time's colon splits tokens (“15”, pause, “30”).
                    // Retract the whole time and its frame, not just minutes.
                    let slot = if replacement == AnchorKind::Number
                        && i >= 2
                        && tokens[i - 1] == Token::Pause
                        && gap(text, tokens, i - 1).trim() == ":"
                        && matches!(&tokens[i - 2], Token::Word(hour, _)
                            if hour.chars().all(|c| c.is_ascii_digit()))
                    {
                        i - 2
                    } else {
                        i
                    };
                    let frame = slot.checked_sub(1).filter(|&j| {
                        matches!(&tokens[j], Token::Word(frame, _)
                            if right.first() == Some(&frame.as_str())
                                && ["um", "am", "an", "im", "in", "auf", "für", "at", "on", "to", "for"].contains(&frame.as_str()))
                    });
                    return Some(frame.unwrap_or(slot));
                }
            }
        }
    }
    None
}

/// Match `cue` starting at token `start`; returns the index after the cue.
fn match_at(tokens: &[Token], start: usize, cue: &[&str]) -> Option<usize> {
    let mut i = start;
    for (n, expected) in cue.iter().enumerate() {
        if n > 0 {
            while tokens.get(i) == Some(&Token::Pause) {
                i += 1;
            }
        }
        match tokens.get(i) {
            Some(Token::Word(w, _)) if w == expected => i += 1,
            _ => return None,
        }
    }
    Some(i)
}

/// The first self-correction cue in `text` (its words joined by spaces), or
/// `None`. See the module docs for the gating rules.
#[cfg(test)]
fn detect_cue(text: &str) -> Option<String> {
    find_cues(text).into_iter().next().map(|(cue, _, _)| cue)
}

pub(super) fn words(text: &str) -> Vec<String> {
    tokenize(text)
        .into_iter()
        .filter_map(|token| match token {
            Token::Word(word, _) => Some(word),
            Token::Pause => None,
        })
        .collect()
}

fn quoted_context(text: &str, tokens: &[Token], start: usize) -> bool {
    let Token::Word(_, offset) = &tokens[start] else {
        return false;
    };
    let prefix = text[..*offset].trim_end();
    if let Some(quote) = prefix.chars().next_back() {
        if matches!(quote, '\"' | '\'' | '„' | '“' | '‘' | '«' | '‹')
            && prefix[..prefix.len() - quote.len_utf8()]
                .trim_end()
                .ends_with(':')
        {
            return true;
        }
    }
    tokens[..start]
        .iter()
        .rev()
        .filter_map(|token| match token {
            Token::Word(word, _) => Some(word.as_str()),
            Token::Pause => None,
        })
        .take(3)
        .any(|word| ["said", "sagte", "asked", "fragte"].contains(&word))
}

/// Every cue: joined words, deletable word positions, and earliest retraction
/// word position (same tokenizer as the over-edit guard).
pub(super) fn find_cues(text: &str) -> Vec<(String, Range<usize>, usize)> {
    let tokens = tokenize(text);
    let mut found = Vec::new();
    let mut cue_end = 0;
    for start in 0..tokens.len() {
        // A multiword cue owns its words: “nein warte” must not also
        // become an inline “warte” with a second, overlapping retraction.
        if start == 0 || start < cue_end || quoted_context(text, &tokens, start) {
            continue;
        }
        for (cue, kind) in CUES {
            let Some(end) = match_at(&tokens, start, cue) else {
                continue;
            };
            let pause_after = tokens.get(end) == Some(&Token::Pause);
            // The cue must replace something: a word must follow it, unless
            // it retracts the previous clause outright ("…. Scratch that.").
            let word_follows = tokens[end..].iter().any(|t| matches!(t, Token::Word(_, _)));
            let pause_before = start >= 2 && tokens[start - 1] == Token::Pause;
            let inline_from = (*kind == Kind::AnchoredWait)
                .then(|| inline_wait_retraction(text, &tokens, start, end))
                .flatten();
            // “Montag beziehungsweise Dienstag” is a slot repair even when
            // ASR omitted the pause. Ordinary alternatives (tea/coffee) are
            // not slots; no other ambiguous cue gets this exception.
            let inline_alternative = !pause_before
                && matches!(*cue, ["beziehungsweise"] | ["bzw"])
                && start > 0
                && matches!((&tokens[start - 1], tokens.get(if pause_after { end + 1 } else { end })),
                    (Token::Word(left, _), Some(Token::Word(right, _)))
                        if left != right && slot_kind(left).is_some()
                            && slot_kind(left) == slot_kind(right));
            if !pause_before && inline_from.is_none() && !inline_alternative {
                continue;
            }
            // Questions separate question/answer, including inside a multiword
            // cue (“No? Wait …”); they never license a self-repair.
            if (pause_before && gap(text, &tokens, start - 1).contains('?'))
                || (start..end)
                    .any(|i| tokens[i] == Token::Pause && gap(text, &tokens, i).contains('?'))
            {
                continue;
            }
            let mut retract_from = inline_from;
            let counts = match kind {
                Kind::Strong => word_follows,
                Kind::Delimited => pause_after,
                Kind::Weak => {
                    pause_after
                        && (tokens[end..]
                            .iter()
                            .filter_map(|t| match t {
                                Token::Word(w, _) => Some(w),
                                Token::Pause => None,
                            })
                            .take(3)
                            .any(|w| is_anchor(w))
                            || restates_around(&tokens, start, end))
                }
                Kind::Repair | Kind::Restatement => {
                    retract_from = restatement_from(&tokens, start, end);
                    if retract_from.is_none() && *kind == Kind::Repair && pause_after {
                        retract_from = Some(clause_before(&tokens, start - 1).start);
                    }
                    retract_from.is_some()
                }
                Kind::AnchoredWait => {
                    let Token::Word(_, offset) = &tokens[start] else {
                        continue;
                    };
                    inline_from.is_some()
                        || (pause_before
                            && text[..*offset]
                                .trim_end()
                                .ends_with(['.', ',', ';', '—', '–', '-', '\n'])
                            && (matching_wait_anchors(&tokens, start, end)
                                || restates_around(&tokens, start, end)))
                }
            };
            if counts {
                let before = tokens[..start]
                    .iter()
                    .filter(|t| matches!(t, Token::Word(_, _)))
                    .count();
                let from = retract_from.map_or(0, |from| {
                    tokens[..from]
                        .iter()
                        .filter(|t| matches!(t, Token::Word(_, _)))
                        .count()
                });
                found.push((cue.join(" "), before..before + cue.len(), from));
                cue_end = end;
                break;
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_self_corrections() {
        let cases = [
            (
                "Wir treffen uns um drei, nein warte, um vier Uhr.",
                "nein warte",
            ),
            ("Treffen um drei. Nein, warte, um vier.", "nein warte"),
            ("Schick es an Peter, ich meine, an Paul.", "ich meine"),
            ("Um drei, ich meine, um vier.", "ich meine"),
            (
                "Bring zwei Flaschen, Korrektur, drei Flaschen.",
                "korrektur",
            ),
            ("Ruf Peter an, vergiss das, ruf Paul an.", "vergiss das"),
            (
                "Let's ship on Monday, scratch that, on Tuesday.",
                "scratch that",
            ),
            ("Let's ship on Monday. Scratch that.", "scratch that"),
            ("I'll bring two, sorry, I meant three.", "sorry i meant"),
            ("Meet at 3, no wait, at 4.", "no wait"),
            ("Send it to Anna, I mean, to Mark.", "i mean"),
            ("Use the red one - no wait - the blue one.", "no wait"),
        ];
        for (text, cue) in cases {
            assert_eq!(detect_cue(text).as_deref(), Some(cue), "input: {text}");
        }
    }

    #[test]
    fn ordinary_prose_does_not_trigger() {
        for text in [
            "Ich meine, das ist eine gute Idee.",
            "Das ist, ich meine, ziemlich gut.",
            "Ich meine das ernst.",
            "Und vergiss das Brot nicht.",
            "Vergiss das nicht, okay?",
            "Die Korrektur des Textes ist fertig.",
            "There is no wait time today.",
            "I mean it.",
            "Well, I mean, it's fine.",
            "I need to scratch that itch.",
            "Actually no one came.",
            "Nein, warte nicht auf mich.",
            "No wait.",
            "",
        ] {
            assert_eq!(detect_cue(text), None, "input: {text}");
        }
    }

    #[test]
    fn bare_wait_with_matching_clause_anchors_triggers() {
        for (text, cue) in [
            ("Wir treffen uns um drei. Warte um vier.", "warte"),
            ("Wir treffen uns um drei, warte, um vier.", "warte"),
            ("Wir treffen uns um 15:30; Moment um 16:30.", "moment"),
            ("Meet on Monday — Wait on Tuesday.", "wait"),
            ("Treffen im Januar. Moment im Februar.", "moment"),
            ("Schick es an Anna. Warte an Paul.", "warte"),
            ("Meet at three. WAIT at four.", "wait"),
            ("Meet at three. Wait make it four.", "wait"),
        ] {
            assert_eq!(detect_cue(text).as_deref(), Some(cue), "{text}");
        }
    }

    #[test]
    fn bare_wait_without_matching_local_anchors_does_not_trigger() {
        for text in [
            "Warte auf mich.",
            "Moment mal, das stimmt.",
            "Warte, ich hole das Buch.",
            "Sie sagte: Warte um vier.",
            "Warte um vier.",
            "Moment on Monday.",
            "Wait for Anna.",
            "Treffen um drei. Warte auf mich.",
            "Treffen um drei. Moment mal, das stimmt.",
            "Treffen um drei. Warte, ich hole das Buch.",
            "Treffen um drei. Sie sagte: Warte um vier.",
            "Meet at three. She said, Wait at four.",
            "Meet at three: Wait at four.",
            "Meet at three. Wait on Monday.",
            "Meet on Monday. Wait in January.",
            "Meet Anna. Wait at four.",
            "Meet at three. Nothing to retract. Wait at four.",
            "Meet at three. Wait let me think four.",
            "Meet at three. Wait here. Four people arrived.",
        ] {
            assert_eq!(detect_cue(text), None, "{text}");
        }
    }

    #[test]
    fn bare_wait_cue_range_excludes_replacement_words() {
        assert_eq!(
            find_cues("Wir treffen uns um drei. Warte um vier."),
            vec![("warte".to_string(), 5..6, 0)]
        );
    }

    #[test]
    fn tokenizer_collapses_pauses_and_keeps_apostrophes() {
        assert_eq!(
            tokenize("I'm here, ... no"),
            vec![
                Token::Word("i'm".into(), 0),
                Token::Word("here".into(), 4),
                Token::Pause,
                Token::Word("no".into(), 14),
            ]
        );
    }

    #[test]
    fn colon_and_opening_quote_do_not_trigger() {
        for (open, close) in [
            ('"', '"'),
            ('\'', '\''),
            ('„', '“'),
            ('“', '”'),
            ('‘', '’'),
            ('«', '»'),
            ('‹', '›'),
        ] {
            for cue in [
                "no wait, bring four",
                "scratch that, bring four",
                "ich meine, um vier",
            ] {
                let text = format!("The message reads: {open}{cue}.{close}");
                assert_eq!(detect_cue(&text), None, "{text}");
            }
        }
    }

    #[test]
    fn reported_speech_within_three_words_does_not_trigger() {
        for verb in ["said", "sagte", "asked", "fragte"] {
            for between in ["", " to", " to me"] {
                let text = format!("She {verb}{between}, no wait, bring four bottles.");
                assert_eq!(detect_cue(&text), None, "{text}");
            }
        }
        assert_eq!(
            detect_cue("She said to everyone yesterday, no wait, bring four bottles.").as_deref(),
            Some("no wait")
        );
    }

    #[test]
    fn ambiguous_weak_cue_anchors_do_not_trigger() {
        for word in ["one", "may", "march", "today", "heute", "morgen"] {
            assert!(!is_anchor(word), "{word}");
            for cue in ["I mean", "ich meine"] {
                let text = format!("Well, {cue}, {word} seems fine.");
                assert_eq!(detect_cue(&text), None, "{text}");
            }
        }
    }

    #[test]
    fn explicit_numbers_dates_and_first_names_still_anchor_weak_cues() {
        assert!(crate::correction_learning::lexicon::is_name("lena"));
        for word in NUMBER_WORDS
            .iter()
            .chain(WEEKDAYS)
            .chain(MONTHS)
            .copied()
            .chain(["4", "12", "paul", "anna", "lena"])
        {
            assert!(is_anchor(word), "{word}");
            let text = format!("Meet there, I mean, {word}.");
            assert_eq!(detect_cue(&text).as_deref(), Some("i mean"), "{text}");
        }
    }
    #[test]
    fn bare_no_after_period_needs_a_restatement() {
        for (text, cue) in [
            ("Schick das an Tom. Nein, an Tim.", "nein"),
            ("Schick das an Tom. Nein an Tim.", "nein"),
            ("Schick das an Tom. Nee, an Tim.", "nee"),
            ("Schick das an Tom. Nee an Tim.", "nee"),
            ("Send it to Tom. No, to Tim.", "no"),
            ("Send it to Tom. No to Tim.", "no"),
            ("Ich komme morgen. Nein, übermorgen.", "nein"),
            ("Ich komme morgen. Nein übermorgen.", "nein"),
            ("Treffen am Montag. Nein, Dienstag.", "nein"),
            ("Treffen am Montag. Nein Dienstag.", "nein"),
            ("Treffen am Montag. Nee Dienstag.", "nee"),
            ("Meet on Monday. No Tuesday.", "no"),
        ] {
            assert_eq!(detect_cue(text).as_deref(), Some(cue), "{text}");
        }
        for text in [
            "Ist es Montag? Nein, Dienstag.",
            "Ist es Montag? Nein Dienstag.",
            "Ist es Montag?. Nein, Dienstag.",
            "Ist es Montag?. Nein Dienstag.",
            "Das war gut. Nein, wirklich.",
            "Das war gut. Nein wirklich.",
            "Nein danke.",
            "Schick das an Tom. Nein wirklich an Tim.",
            "Treffen am Montag. Nein erst Dienstag.",
            "Send it to Tom. No, really.",
            "Send it to Tom. No really.",
        ] {
            assert_eq!(detect_cue(text), None, "{text}");
        }
    }

    #[test]
    fn inline_wait_requires_matching_local_slots() {
        for (text, cue) in [
            ("Wir treffen uns um drei und dann warte um vier.", "warte"),
            ("Treffen Montag und moment Dienstag.", "moment"),
            ("Meet in January and wait in February.", "wait"),
            ("Ich komme morgen und warte übermorgen.", "warte"),
            ("Meet at 15:30 and wait at 16:30.", "wait"),
        ] {
            assert_eq!(detect_cue(text).as_deref(), Some(cue), "{text}");
        }
        for text in [
            "Dann warte ich um vier auf dich.",
            "Warte auf mich um drei.",
            "Ich warte seit drei Uhr, um vier gehe ich.",
            "Meet at three and wait on Monday.",
            "Meet at three and wait let me think four.",
            "Meet at three. And then wait at four.",
            "Meet at three and bring the tickets for everyone then wait at four.",
            "Meet at three and wait I arrive at four.",
            "Meet at three and wait. Four people arrive.",
            "Meet at three and she said wait at four.",
        ] {
            assert_eq!(detect_cue(text), None, "{text}");
        }
    }
    #[test]
    fn expanded_negations_require_an_immediate_restatement() {
        for cue in ["nein", "nee", "ne", "nö", "no", "nope", "nah"] {
            for boundary in [".", ",", ";", "–", "…", "...", "\n"] {
                for after in [" ", ", "] {
                    let text = format!("Ich komme morgen{boundary} {cue}{after}übermorgen.");
                    assert_eq!(detect_cue(&text).as_deref(), Some(cue), "{text}");
                }
            }
            for text in [
                format!("{cue}, übermorgen."),
                format!("Ich komme morgen. {cue}, wirklich."),
                format!("Ist es morgen? {cue}, übermorgen."),
            ] {
                assert_eq!(detect_cue(&text), None, "{text}");
            }
        }
        // “Naja” remains an ordinary discourse word.
        assert_eq!(detect_cue("Ich komme morgen. Naja, übermorgen."), None);
    }

    #[test]
    fn expanded_correction_lexicon_and_question_veto() {
        for cue in [
            "korrigiere",
            "korrigier",
            "korrektur",
            "ich korrigiere",
            "berichtige",
            "besser gesagt",
            "genauer gesagt",
            "oder besser",
            "oder vielmehr",
            "vielmehr",
            "beziehungsweise",
            "bzw.",
            "sorry",
            "entschuldigung",
            "pardon",
            "correction",
            "rather",
            "or rather",
            "make that",
            "let me rephrase",
            "sorry I meant",
        ] {
            let expected = cue.trim_end_matches('.').to_lowercase();
            for after in [" ", ", "] {
                let text = format!("Ich komme morgen. {cue}{after}übermorgen.");
                assert_eq!(
                    detect_cue(&text).as_deref(),
                    Some(expected.as_str()),
                    "{text}"
                );
            }
            let question = format!("Ist es morgen? {cue}, übermorgen.");
            assert_eq!(detect_cue(&question), None, "{question}");
        }
        for text in [
            "Ist es Montag? Nein, warte, Dienstag.",
            "Ist es Montag? I mean, Tuesday.",
            "Ist es Montag? Scratch that, Tuesday.",
            "Meet Monday, no? Wait, Tuesday.",
        ] {
            assert_eq!(detect_cue(text), None, "{text}");
        }
    }

    #[test]
    fn explicit_corrections_can_be_delimited_without_a_repeated_slot() {
        for cue in [
            "korrigiere",
            "korrigier",
            "korrektur",
            "ich korrigiere",
            "berichtige",
            "besser gesagt",
            "genauer gesagt",
            "oder besser",
            "oder vielmehr",
            "correction",
            "make that",
            "let me rephrase",
        ] {
            let text = format!("Bring Wein, {cue}, bring Bier.");
            assert_eq!(detect_cue(&text).as_deref(), Some(cue), "{text}");
        }
    }

    #[test]
    fn ambiguous_cues_and_normal_requests_stay_untouched() {
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
            "Wir warten, rather than waiting, we go now.",
            "Das ist gut, sorry, ich bin spät dran.",
            "Das ist gut, beziehungsweise, es passt schon.",
            "Das ist gut, vielmehr, es passt schon.",
            "Das ist gut, pardon, ich bin spät dran.",
        ] {
            assert_eq!(detect_cue(text), None, "{text}");
        }
    }

    #[test]
    fn restatement_can_replace_an_internal_slot_or_repeat_its_frame() {
        for (left, right, from) in [
            (vec!["ruf", "anna", "an"], vec!["lena"], 1),
            (vec!["ruf", "anna", "an"], vec!["ruf", "lena", "an"], 0),
            (vec!["at", "five", "tonight"], vec!["six"], 1),
            (vec!["montag", "passt"], vec!["dienstag"], 0),
            (vec!["im", "januar", "starten"], vec!["februar"], 1),
            (vec!["um", "15", "kommen"], vec!["16"], 1),
        ] {
            assert_eq!(
                restates(&left, &right, ALL_SLOTS),
                Some(from),
                "{left:?} → {right:?}"
            );
        }
        assert_eq!(
            restates(&["guten", "morgen"], &["übermorgen"], ALL_SLOTS),
            None
        );
        assert_eq!(restates(&["at", "five"], &["well", "six"], ALL_SLOTS), None);
    }

    #[test]
    fn inline_alternatives_need_adjacent_same_class_slots() {
        for cue in ["beziehungsweise", "bzw."] {
            let input = format!("Treffen am Montag {cue} Dienstag.");
            assert_eq!(
                find_cues(&input),
                vec![(cue.trim_end_matches('.').to_string(), 3..4, 2)]
            );
            for input in [
                format!("Ich nehme Tee {cue} Kaffee."),
                format!("Treffen am Montag {cue} erst Dienstag."),
                format!("Guten Morgen {cue} übermorgen."),
                format!("Treffen am Montag {cue} vier."),
            ] {
                assert!(find_cues(&input).is_empty(), "{input}");
            }
        }
    }
}
