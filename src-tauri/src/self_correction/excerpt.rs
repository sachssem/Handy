//! The part of a dictation the cleanup model gets.
//!
//! The model writes its answer out word by word, so a pass over the whole
//! dictation takes as long as the dictation is long: the on-device model
//! needed 1.8 s for 200 characters and ran into the 3 s cap from 440
//! characters on, which is every dictation over half a minute. The
//! disfluencies sit in a sentence or two, so only those go to the model and
//! its answer is put back in their place.

use super::disfluency::Span;
use super::guard::word_ranges;
use std::ops::Range;

/// The longest excerpt, in characters. The on-device model answers this much
/// in about 2.2 s.
pub(super) const MAX_CHARS: usize = 280;

/// Words kept on each side of a disfluency when its sentence alone is longer
/// than [`MAX_CHARS`].
const WORD_PAD: usize = 10;

/// Byte offsets at which a sentence starts; the first is 0.
fn sentence_starts(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut ended = false;
    let mut chars = text.char_indices().peekable();
    while let Some((offset, c)) = chars.next() {
        if c == '\n' {
            ended = true;
        } else if matches!(c, '.' | '!' | '?' | '…') {
            ended = chars.peek().is_none_or(|(_, next)| next.is_whitespace());
        } else if ended && !c.is_whitespace() {
            starts.push(offset);
            ended = false;
        }
    }
    starts
}

/// The byte range of `text` to send for `spans` (word positions in `text`):
/// the sentences that hold them, the whole text when that is short enough.
pub(super) fn range(text: &str, spans: &[Span]) -> Range<usize> {
    let whole = 0..text.len();
    let words = word_ranges(text);
    if text.chars().count() <= MAX_CHARS || words.is_empty() || spans.is_empty() {
        return whole;
    }
    let last = words.len() - 1;
    let starts = sentence_starts(text);
    let sentence_of = |word: usize| starts.partition_point(|s| *s <= words[word].start) - 1;
    let bytes = |first: usize, last_sentence: usize| {
        let end = starts.get(last_sentence + 1).copied().unwrap_or(text.len());
        starts[first]..starts[first] + text[starts[first]..end].trim_end().len()
    };
    let fits = |range: &Range<usize>| text[range.clone()].chars().count() <= MAX_CHARS;

    // Each disfluency's first and last word (the word after it included: a
    // repair's replacement follows its cue) and the sentences they are in.
    let mut groups: Vec<(usize, usize, usize, usize)> = spans
        .iter()
        .map(|span| {
            let lo = span
                .retract_from
                .map_or(span.words.start, |from| from.min(span.words.start))
                .min(last);
            let hi = span.words.end.min(last).max(lo);
            let mut first = sentence_of(lo);
            // What a disfluency at a sentence's start takes back is in the
            // sentence before it.
            if first > 0 && (lo == 0 || sentence_of(lo - 1) != first) {
                first -= 1;
            }
            (first, sentence_of(hi), lo, hi)
        })
        .collect();
    groups.sort_unstable();

    let all = bytes(
        groups[0].0,
        groups.iter().map(|g| g.1).max().unwrap_or(groups[0].1),
    );
    if fits(&all) {
        return all;
    }
    // Too far apart: the first disfluency's sentences, the rest stay as said.
    let (first, last_sentence, lo, hi) = groups[0];
    let sentences = bytes(first, last_sentence);
    if fits(&sentences) {
        return sentences;
    }
    // One long sentence: the words around the disfluency.
    let start = words[lo.saturating_sub(WORD_PAD)].start;
    let end_word = &words[(hi + WORD_PAD).min(last)];
    let end = end_word.end
        + text[end_word.end..]
            .find(char::is_whitespace)
            .unwrap_or(text.len() - end_word.end);
    start..end
}

#[cfg(test)]
mod tests {
    use super::super::disfluency::detect;
    use super::*;

    fn excerpt(text: &str) -> &str {
        let spans = detect(text, Some("de"));
        assert!(!spans.is_empty(), "no disfluency in {text:?}");
        &text[range(text, &spans)]
    }

    fn padding(sentences: usize) -> String {
        "Das hier ist ein ganz normaler Satz ohne Auffälligkeiten. ".repeat(sentences)
    }

    #[test]
    fn a_short_text_goes_whole() {
        let text = "Das ist ist gut. Und der Rest auch.";
        assert_eq!(excerpt(text), text);
    }

    #[test]
    fn a_long_text_sends_only_the_sentence_with_the_disfluency() {
        let text = format!(
            "{}Dann haben wir wir das eingebaut. {}",
            padding(6),
            padding(6)
        );
        assert_eq!(excerpt(&text), "Dann haben wir wir das eingebaut.");
        // The excerpt on its own still shows the disfluency to the guard.
        assert!(!detect(excerpt(&text), Some("de")).is_empty());
    }

    #[test]
    fn a_repair_over_two_sentences_sends_both() {
        let text = format!(
            "{}Ich komme morgen. Ne, übermorgen. {}",
            padding(6),
            padding(6)
        );
        assert_eq!(excerpt(&text), "Ich komme morgen. Ne, übermorgen.");
    }

    #[test]
    fn disfluencies_far_apart_send_the_first() {
        let text = format!(
            "Das ist ist gut. {}Dann haben wir wir das eingebaut.",
            padding(8)
        );
        assert_eq!(excerpt(&text), "Das ist ist gut.");
    }

    #[test]
    fn a_long_sentence_sends_the_words_around_the_disfluency() {
        let clause = "und dann geht es immer weiter ohne Punkt und Komma ";
        let text = format!(
            "{}und da haben wir wir das eingebaut {}",
            clause.repeat(6),
            clause.repeat(6)
        );
        let part = excerpt(text.trim_end());
        assert!(part.chars().count() <= MAX_CHARS, "{part:?}");
        assert!(part.contains("haben wir wir das"), "{part:?}");
        assert!(!detect(part, Some("de")).is_empty());
    }

    #[test]
    fn the_answer_goes_back_in_the_excerpts_place() {
        let text = format!(
            "{}Dann haben wir wir das eingebaut. {}",
            padding(6),
            padding(6)
        );
        let spans = detect(&text, Some("de"));
        let range = range(&text, &spans);
        let mut cleaned = text.clone();
        cleaned.replace_range(range, "Dann haben wir das eingebaut.");
        assert_eq!(cleaned, text.replace("wir wir", "wir"));
    }
}
