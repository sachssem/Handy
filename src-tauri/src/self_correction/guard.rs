//! Over-edit guard for the self-correction LLM pass.
//!
//! A disfluency cleanup only ever *removes* words — fillers, stutters, the
//! retracted part of a self-repair — and only at the detected
//! [`Span`]s. A result that is much shorter, longer, brings in any new word or
//! deletes elsewhere is an over-edit — a rephrase, a translation, an answer
//! to the text — and is discarded in favour of the rules output.
//! Word-level alignment is followed by a verbatim check of retained text.
//! Only whitespace, adjacent punctuation and the first letter at a cut may change.

use super::cues::{tokenize, words, Token};
use super::disfluency::Span;
use similar::{capture_diff_slices_deadline, Algorithm, DiffTag};
use std::ops::Range;
use std::time::{Duration, Instant};

/// The result must keep at least this share of the input's characters.
const MIN_LENGTH_RATIO: f64 = 0.30;
/// …and may not exceed this share (punctuation/casing touch-ups only).
const MAX_LENGTH_RATIO: f64 = 1.10;

/// Clean the raw model answer: a leading `<think>` block, an `Output:` label
/// and wrapping quotes the input did not have are removed.
pub(crate) fn clean_response(input: &str, raw: &str) -> String {
    let mut out = raw.trim();
    if let Some(rest) = out.strip_prefix("<think>") {
        if let Some(end) = rest.find("</think>") {
            out = rest[end + "</think>".len()..].trim();
        }
    }
    for label in ["Output:", "Ausgabe:"] {
        if let Some(rest) = out.strip_prefix(label) {
            out = rest.trim();
        }
    }
    let input = input.trim();
    for (open, close) in [('"', '"'), ('„', '“'), ('“', '”'), ('\'', '\'')] {
        if out.len() >= 2
            && out.starts_with(open)
            && out.ends_with(close)
            && !input.starts_with(open)
        {
            out = out[open.len_utf8()..out.len() - close.len_utf8()].trim();
        }
    }
    out.replace(['\u{200B}', '\u{200C}', '\u{200D}', '\u{FEFF}'], "")
}

/// Accept `output` as the cleaned-up `input` with disfluencies `spans`, or
/// name why not (`empty`, `unchanged`, `length_ratio`, `added_words`,
/// `unrelated_deletion`, `retained_text_changed`).
pub(crate) fn check(input: &str, output: &str, spans: &[Span]) -> Result<(), &'static str> {
    if output.trim().is_empty() {
        return Err("empty");
    }
    if output == input {
        return Err("unchanged");
    }
    let ratio = output.chars().count() as f64 / input.chars().count().max(1) as f64;
    if !(MIN_LENGTH_RATIO..=MAX_LENGTH_RATIO).contains(&ratio) {
        return Err("length_ratio");
    }
    let mut input_words = words(input);
    let mut out_words = words(output);
    // Align repeated frame words from the end: “um drei … um vier” keeps
    // the replacement's “um”, rather than treating it as a deleted word.
    input_words.reverse();
    out_words.reverse();
    let changes = capture_diff_slices_deadline(
        Algorithm::Myers,
        &input_words,
        &out_words,
        Some(Instant::now() + Duration::from_millis(50)),
    );
    let mut kept = Vec::new();
    for change in changes {
        if change.tag() == DiffTag::Equal {
            kept.extend(
                change
                    .old_range()
                    .zip(change.new_range())
                    .map(|(old, new)| (input_words.len() - old - 1, out_words.len() - new - 1)),
            );
            continue;
        }
        // No new words at all — not even a re-inflected one after a cut.
        if !change.new_range().is_empty() {
            return Err("added_words");
        }
        let deleted = change.old_range();
        let deleted = input_words.len() - deleted.end..input_words.len() - deleted.start;
        if !deletion_allowed(deleted, spans) {
            return Err("unrelated_deletion");
        }
    }
    kept.reverse();
    if !retained_text_matches(input, output, &kept) {
        return Err("retained_text_changed");
    }
    Ok(())
}

/// Byte ranges use the same tokenizer as the word alignment, including its
/// handling of contractions and Unicode letters.
fn word_ranges(text: &str) -> Vec<Range<usize>> {
    tokenize(text)
        .into_iter()
        .filter_map(|token| match token {
            Token::Word(_, start) => {
                let mut end = start;
                for (offset, c) in text[start..].char_indices() {
                    if c.is_alphanumeric()
                        || (matches!(c, '\'' | '’')
                            && offset > 0
                            && text[start + offset + c.len_utf8()..]
                                .chars()
                                .next()
                                .is_some_and(char::is_alphanumeric))
                    {
                        end = start + offset + c.len_utf8();
                    } else {
                        break;
                    }
                }
                Some(start..end)
            }
            Token::Pause => None,
        })
        .collect()
}

fn first_letter_matches(before: &str, after: &str) -> bool {
    let (Some(a), Some(b)) = (before.chars().next(), after.chars().next()) else {
        return false;
    };
    a.is_alphabetic()
        && b.is_alphabetic()
        && a.to_lowercase().eq(b.to_lowercase())
        && before[a.len_utf8()..] == after[b.len_utf8()..]
}

fn cut_mark(c: char) -> bool {
    matches!(c, ',' | '.' | '-' | '–' | '—')
}

/// Only the mark nearest the deletion on either side is optional. Symbols,
/// quotes and all other punctuation on the retained side remain significant.
fn cut_gap_matches(left: &str, right: &str, actual: &str) -> bool {
    let left = left.trim_end();
    let right = right.trim_start();
    let shorter_left = left
        .chars()
        .next_back()
        .filter(|c| cut_mark(*c))
        .map(|c| &left[..left.len() - c.len_utf8()]);
    let shorter_right = right
        .chars()
        .next()
        .filter(|c| cut_mark(*c))
        .map(|c| &right[c.len_utf8()..]);
    let normalize = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    [Some(left), shorter_left].into_iter().flatten().any(|l| {
        [Some(right), shorter_right].into_iter().flatten().any(|r| {
            // At the cut, a whitespace run may become one space or disappear.
            [format!("{l}{r}"), format!("{l} {r}")]
                .iter()
                .any(|candidate| normalize(candidate) == normalize(actual))
        })
    })
}

fn retained_text_matches(input: &str, output: &str, kept: &[(usize, usize)]) -> bool {
    let original = word_ranges(input);
    let result = word_ranges(output);
    let mut previous: Option<(usize, usize)> = None;
    for &(old, new) in kept {
        let before = &original[old];
        let after = &result[new];
        let at_cut = previous.map_or(old > 0, |(prev, _)| old > prev + 1);
        let raw_before = &input[before.clone()];
        let raw_after = &output[after.clone()];
        if raw_before != raw_after
            && !((new == 0 || at_cut) && first_letter_matches(raw_before, raw_after))
        {
            return false;
        }
        let (old_end, new_end, first_deleted) =
            previous.map_or((0, 0, 0), |(o, n)| (original[o].end, result[n].end, o + 1));
        let actual = &output[new_end..after.start];
        if at_cut {
            if !cut_gap_matches(
                &input[old_end..original[first_deleted].start],
                &input[original[old - 1].end..before.start],
                actual,
            ) {
                return false;
            }
        } else if input[old_end..before.start] != *actual {
            return false;
        }
        previous = Some((old, new));
    }
    let Some((old, new)) = previous else {
        return false;
    };
    let actual = &output[result[new].end..];
    if old + 1 < original.len() {
        let left = &input[original[old].end..original[old + 1].start];
        let right = &input[original[original.len() - 1].end..];
        return cut_gap_matches(left, right, actual)
            || (matches!(actual, "." | "!" | "?")
                && (!left.trim().is_empty() || !right.trim().is_empty())
                && [left, right].iter().all(|gap| {
                    let gap = gap.trim();
                    gap.chars().count() <= 1 && gap.chars().all(cut_mark)
                }));
    }
    let suffix = &input[original[old].end..];
    if suffix == actual {
        return true;
    }
    // The sentence-final mark on a word immediately following the cut can
    // be dropped or become another sentence-final mark (the slot-repair case).
    let follows_cut =
        kept.len() == 1 && old > 0 || kept.len() > 1 && old > kept[kept.len() - 2].0 + 1;
    follows_cut
        && matches!(suffix, "," | "." | "-" | "–" | "—")
        && matches!(actual, "" | "." | "!" | "?")
}

/// A removed word run is allowed when every word lies in a span's deletable
/// words, or belongs to a retracted part: a contiguous stretch that starts no
/// earlier than a repair's `retract_from` and ends at or within two words of
/// the repair on its preceding side. Replacement words after the cue are
/// never part of the retraction.
fn deletion_allowed(run: Range<usize>, spans: &[Span]) -> bool {
    let covered = |i: usize| spans.iter().any(|span| span.words.contains(&i));
    let mut i = run.start;
    while i < run.end {
        if covered(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < run.end && !covered(i) {
            i += 1;
        }
        let retracted = spans.iter().any(|span| {
            span.retract_from.is_some_and(|from| {
                start >= from && i >= span.words.start.saturating_sub(2) && i <= span.words.start
            })
        });
        if !retracted {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::super::disfluency::detect;
    use super::*;

    /// The guard as the pass runs it: spans detected on the input.
    fn guard(input: &str, output: &str) -> Result<(), &'static str> {
        check(input, output, &detect(input, Some("de")))
    }

    #[test]
    fn accepts_a_minimal_correction() {
        assert_eq!(
            guard(
                "Wir treffen uns um drei, nein warte, um vier Uhr.",
                "Wir treffen uns um vier Uhr."
            ),
            Ok(())
        );
        // Not even one re-inflected word is tolerated.
        assert_eq!(
            guard(
                "Bring den Hammer, nein warte, die Zange.",
                "Bring die Zangen."
            ),
            Err("added_words")
        );
    }

    #[test]
    fn accepts_slot_repair_across_a_sentence_boundary() {
        let input = "Ich komme morgen. Ne, übermorgen.";
        let spans = detect(input, Some("de"));
        assert_eq!(spans[0].words, 3..4);
        assert_eq!(spans[0].retract_from, Some(2));
        for output in [
            "Ich komme übermorgen.",
            "Ich komme übermorgen",
            "Ich komme übermorgen!",
        ] {
            assert_eq!(check(input, output, &spans), Ok(()), "{output}");
        }
        assert_eq!(
            guard(input, "Ich komme nächsten übermorgen."),
            Err("added_words")
        );
        assert_eq!(guard(input, "Ich übermorgen."), Err("unrelated_deletion"));
    }

    #[test]
    fn accepts_correction_when_asr_dropped_nein() {
        assert_eq!(
            guard(
                "Wir treffen uns um drei. Warte um vier.",
                "Wir treffen uns um vier."
            ),
            Ok(())
        );
    }

    #[test]
    fn rejects_over_edits() {
        let input = "Let's ship on Monday, scratch that, on Tuesday.";
        assert_eq!(guard(input, input), Err("unchanged"));
        assert_eq!(guard(input, "  "), Err("empty"));
        assert_eq!(guard(input, "Tuesday."), Err("length_ratio"));
        assert_eq!(
            guard(
                input,
                "Let's ship on Tuesday, as discussed with the whole team today."
            ),
            Err("length_ratio")
        );
        assert_eq!(
            guard(input, "We will release it on Tuesday then."),
            Err("added_words")
        );
    }

    #[test]
    fn rejects_changes_to_retained_characters() {
        for (input, output) in [
            (
                "Überweise 1.000 Euro, öhm, an Marc.",
                "Überweise 1,000 Euro an Marc.",
            ),
            (
                "Überweise 1.000 Euro, öhm, an Marc.",
                "Überweise 1.000 euro an Marc.",
            ),
            (
                "Überweise 1.000 Euro, öhm, an Marc.",
                "Überweise 1.000 Euro an marc.",
            ),
            (
                "Überweise 1.000 € Euro, öhm, an Marc.",
                "Überweise 1.000 Euro an Marc.",
            ),
            (
                "Bitte: überweise, öhm, an Marc.",
                "Bitte überweise an Marc.",
            ),
            (
                "Überweise 1.000 Euro, öhm, an Marc.",
                "Überweise 1.000 Euro an Marc!",
            ),
            (
                "Wir  brauchen, öhm, drei Tickets.",
                "Wir brauchen drei Tickets.",
            ),
            (
                "Wir brauchen, öhm, drei Tickets.",
                "Wir brauchen drei TICKETS.",
            ),
            (
                "Wir brauchen, öhm, drei Tickets.",
                "Wir brauchen; drei Tickets.",
            ),
            (
                "Wir brauchen,, öhm, drei Tickets.",
                "Wir brauchen drei Tickets.",
            ),
            ("Ich komme morgen. Ne, übermorgen", "Ich komme übermorgen!"),
            (" Wir brauchen, öhm, Tickets.", "Wir brauchen Tickets."),
        ] {
            assert_eq!(
                guard(input, output),
                Err("retained_text_changed"),
                "{input} → {output}"
            );
        }
    }

    #[test]
    fn accepts_only_local_boundary_touchups() {
        for (input, output) in [
            (
                "Überweise 1.000 Euro, öhm, an Marc.",
                "Überweise 1.000 Euro an Marc.",
            ),
            (
                "Überweise 1.000 € Euro, öhm, an Marc.",
                "Überweise 1.000 € Euro an Marc.",
            ),
            (
                "Wir brauchen, öhm, drei Tickets.",
                "Wir brauchen, drei Tickets.",
            ),
            ("Öhm, wir brauchen Tickets.", "Wir brauchen Tickets."),
            (
                "Wir brauchen, öhm, Drei Tickets.",
                "Wir brauchen drei Tickets.",
            ),
            (
                "Wir brauchen, öhm, drei Tickets.",
                "Wir brauchen\t drei Tickets.",
            ),
            (
                "wir brauchen, öhm, drei Tickets.",
                "Wir brauchen drei Tickets.",
            ),
            ("Bitte bring Tickets, öhm", "Bitte bring Tickets."),
        ] {
            assert_eq!(guard(input, output), Ok(()), "{input} → {output}");
        }
    }

    #[test]
    fn cleans_model_wrappers() {
        let input = "Um drei, nein warte, um vier.";
        assert_eq!(clean_response(input, "\"Um vier.\""), "Um vier.");
        assert_eq!(clean_response(input, "Output: Um vier."), "Um vier.");
        assert_eq!(
            clean_response(input, "<think>hmm</think>\n Um vier.\u{200B}"),
            "Um vier."
        );
        // Quotes the input itself opens with stay.
        assert_eq!(
            clean_response("\"Hallo\", nein warte, \"Hi\"", "\"Hi\""),
            "\"Hi\""
        );
    }

    #[test]
    fn rejects_deletions_elsewhere_even_with_a_valid_local_correction() {
        let input = "Please keep the confidential attachment for our team. Meet at three, no wait, at four tomorrow afternoon.";
        for output in [
            "Please keep the attachment for our team. Meet at four tomorrow afternoon.",
            "Please keep the confidential attachment for our team. Meet at four afternoon.",
        ] {
            assert_eq!(guard(input, output), Err("unrelated_deletion"), "{output}");
        }
    }

    #[test]
    fn deletion_must_end_within_two_words_of_the_cue() {
        // Removing red ends two retained words before the cue; removing
        // Please ends four words before it, so only the former is local.
        let input = "Please bring red bottles tonight, no wait, blue bottles tomorrow.";
        assert_eq!(
            guard(
                input,
                "Please bring bottles tonight, blue bottles tomorrow."
            ),
            Ok(())
        );
        assert_eq!(
            guard(input, "bring red bottles tonight, blue bottles tomorrow."),
            Err("unrelated_deletion")
        );
        let input = "Please bring red bottles for dinner, no wait, blue bottles tomorrow.";
        assert_eq!(
            guard(
                input,
                "Please bring bottles for dinner, blue bottles tomorrow."
            ),
            Err("unrelated_deletion")
        );
    }

    #[test]
    fn accepts_local_deletions_at_multiple_cues_and_with_contractions() {
        assert_eq!(
            guard(
                "I'll bring three, no wait, four bottles and meet on Monday, scratch that, Tuesday.",
                "I'll bring four bottles and meet on Tuesday."
            ),
            Ok(())
        );
    }

    #[test]
    fn quoted_cues_cannot_authorize_a_deletion() {
        assert_eq!(
            guard(
                "She said: \"no wait, bring four bottles.\"",
                "She said: \"bring four bottles.\""
            ),
            Err("unrelated_deletion")
        );
    }

    #[test]
    fn accepts_deletions_at_every_detected_span() {
        for (input, output) in [
            ("Ich komme morgen, äh, übermorgen.", "Ich komme übermorgen."),
            ("Ich komme morgen, übermorgen.", "Ich komme übermorgen."),
            (
                "Wir brauchen, ähm, drei Tickets.",
                "Wir brauchen drei Tickets.",
            ),
            ("Das ist ist gut.", "Das ist gut."),
            ("Schick das an Tom, nein, an Tim.", "Schick das an Tim."),
            ("Ich wollte – ich muss jetzt los.", "Ich muss jetzt los."),
            (
                "Öhm, wir brauchen ich ich meine Tickets am Montag, am Dienstag.",
                "Wir brauchen meine Tickets am Dienstag.",
            ),
        ] {
            assert_eq!(guard(input, output), Ok(()), "{input} → {output}");
        }
    }

    #[test]
    fn rejects_edits_outside_the_spans() {
        for (input, output) in [
            // The filler goes, but so does an unrelated word.
            ("Wir brauchen, ähm, drei Tickets.", "Wir brauchen Tickets."),
            ("Wir brauchen, ähm, drei Tickets.", "Brauchen drei Tickets."),
            // A filler licenses no retraction without a restatement.
            ("Wir brauchen, ähm, drei Tickets.", "Wir drei Tickets."),
            // A stutter licenses only the repeated words.
            ("Das ist ist wirklich gut.", "Das ist gut."),
            // Nothing detected: nothing may go.
            ("Ich glaube, ich komme später.", "Ich komme später."),
        ] {
            assert_eq!(
                guard(input, output),
                Err("unrelated_deletion"),
                "{input} → {output}"
            );
        }
    }

    #[test]
    fn rejects_paraphrase_and_added_words() {
        for (input, output) in [
            (
                "Wir brauchen, ähm, drei Tickets.",
                "Wir benötigen drei Tickets.",
            ),
            ("Das ist ist gut.", "Das ist super."),
            (
                "Ich komme morgen, äh, übermorgen.",
                "Ich komme erst übermorgen.",
            ),
            ("Schick das an Tom, nein, an Tim.", "Sende das an Tim."),
        ] {
            assert_eq!(
                guard(input, output),
                Err("added_words"),
                "{input} → {output}"
            );
        }
    }
    #[test]
    fn accepts_qwen_repairs_and_rejects_deletions_outside_their_spans() {
        for (input, expected, prefix_deleted, replacement_deleted) in [
            (
                "Ich komme morgen übermorgen.",
                "Ich komme übermorgen.",
                "Komme übermorgen.",
                "Ich komme morgen.",
            ),
            (
                "Schick das an Tom. Nein, an Tim.",
                "Schick das an Tim.",
                "Schick an Tim.",
                "Schick das an Tom.",
            ),
            (
                "Schick das an Tom. Nein an Tim.",
                "Schick das an Tim.",
                "Schick an Tim.",
                "Schick das an Tom.",
            ),
            (
                "Treffen am Montag. Nein Dienstag.",
                "Treffen am Dienstag.",
                "Treffen Dienstag.",
                "Treffen am Montag.",
            ),
            (
                "Wir treffen uns um drei und dann warte um vier.",
                "Wir treffen uns um vier.",
                "Wir treffen um vier.",
                "Wir treffen uns um drei.",
            ),
            (
                "Please meet at 15:30 and wait at 16:30.",
                "Please meet at 16:30.",
                "Please at 16:30.",
                "Please meet at 15:30.",
            ),
        ] {
            assert_eq!(guard(input, expected), Ok(()), "{input}");
            assert_eq!(
                guard(input, prefix_deleted),
                Err("unrelated_deletion"),
                "{prefix_deleted}"
            );
            assert_eq!(
                guard(input, replacement_deleted),
                Err("unrelated_deletion"),
                "{replacement_deleted}"
            );
        }
    }
    #[test]
    fn accepts_generalized_cues_without_licensing_other_edits() {
        for (input, output) in [
            ("Ich komme morgen. Ne, übermorgen.", "Ich komme übermorgen."),
            (
                "Ich komme morgen. Korrigiere übermorgen.",
                "Ich komme übermorgen.",
            ),
            (
                "Treffen am Montag, beziehungsweise Dienstag.",
                "Treffen am Dienstag.",
            ),
            ("Ruf Anna an. Sorry, ruf Lena an.", "Ruf Lena an."),
            ("See you at five, make that six.", "See you at six."),
            (
                "Wir brauchen drei, korrigiere, vier Tickets.",
                "Wir brauchen vier Tickets.",
            ),
            ("See you at five tonight, make that six.", "See you at six."),
        ] {
            assert_eq!(guard(input, output), Ok(()), "{input} → {output}");
        }
        let input = "Ich komme morgen. Korrigiere übermorgen.";
        for output in ["Komme übermorgen.", "Ich komme morgen."] {
            assert_eq!(guard(input, output), Err("unrelated_deletion"), "{output}");
        }
        assert_eq!(
            guard("Ruf Anna an, sorry, Lena.", "Ruf Lena an."),
            Err("added_words")
        );
        assert_eq!(
            guard(
                "Keep the attachment. Bring Wein, Korrektur, bring Bier.",
                "Keep attachment. Bring Bier."
            ),
            Err("unrelated_deletion")
        );
    }
}
