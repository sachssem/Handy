//! Conservative slot-only repairs. More complex disfluencies stay with the LLM.

use super::cues::{slot_kind, tokenize, AnchorKind, Token};
use super::disfluency::Span;
use super::guard;
use std::ops::Range;

/// Frames are prepositions, optionally with their article. The replacement
/// may omit the frame (keep the original) or repeat it exactly (keep the new).
const FRAMES: &[&[&str]] = &[
    &["in", "den"],
    &["in", "der"],
    &["an", "den"],
    &["an", "der"],
    &["auf", "den"],
    &["auf", "der"],
    &["um"],
    &["am"],
    &["an"],
    &["im"],
    &["in"],
    &["auf"],
    &["für"],
    &["zu"],
    &["at"],
    &["on"],
    &["to"],
    &["for"],
];

struct Word<'a> {
    normalized: String,
    raw: &'a str,
    bytes: Range<usize>,
}

fn words(text: &str) -> Vec<Word<'_>> {
    tokenize(text)
        .into_iter()
        .filter_map(|token| {
            let Token::Word(normalized, start) = token else {
                return None;
            };
            let len = text[start..]
                .find(|c: char| !(c.is_alphanumeric() || matches!(c, '\'' | '’')))
                .unwrap_or(text.len() - start);
            Some(Word {
                normalized,
                raw: &text[start..start + len],
                bytes: start..start + len,
            })
        })
        .collect()
}

fn whitespace(text: &str, words: &[Word<'_>], start: usize, end: usize) -> bool {
    (start..end).all(|i| {
        text[words[i].bytes.end..words[i + 1].bytes.start]
            .chars()
            .all(char::is_whitespace)
    })
}

fn frame_before(text: &str, words: &[Word<'_>], slot: usize) -> usize {
    FRAMES
        .iter()
        .find_map(|frame| {
            let start = slot.checked_sub(frame.len())?;
            (words[start..slot]
                .iter()
                .map(|w| w.normalized.as_str())
                .eq(frame.iter().copied())
                && whitespace(text, words, start, slot))
            .then_some(start)
        })
        .unwrap_or(slot)
}

fn cut(text: &str, words: &[Word<'_>], span: &Span) -> Option<(Range<usize>, String)> {
    if span.label != "restart" && !span.label.starts_with("cue:") {
        return None;
    }
    let old = span.words.start.checked_sub(1)?;
    let next = span.words.end;
    let kind = slot_kind(&words.get(old)?.normalized)?;
    let frame = frame_before(text, words, old);
    // Retraction must be precisely a slot and its optional frame, with no
    // trailing words. Broad explicit cues may start at zero.
    let from = span.retract_from?;
    if from != 0 && from != old && from != frame {
        return None;
    }
    // A tokenizer splits clock times at ':'. Never repair only the minutes.
    if old > 0 && text[words[old - 1].bytes.end..words[old].bytes.start].contains(':') {
        return None;
    }
    if old > 0 && words[old - 1].normalized == "guten" && words[old].normalized == "morgen" {
        return None;
    }
    let new_frame = FRAMES.iter().find(|f| {
        words.get(next..next + f.len()).is_some_and(|ws| {
            ws.iter()
                .map(|w| w.normalized.as_str())
                .eq(f.iter().copied())
        })
    });
    let (remove, replacement) = if let Some(new_frame) = new_frame {
        let new_slot = next + new_frame.len();
        if frame == old
            || new_frame.len() != old - frame
            || !words[frame..old]
                .iter()
                .map(|w| &w.normalized)
                .eq(words[next..new_slot].iter().map(|w| &w.normalized))
            || !whitespace(text, words, next, new_slot)
        {
            return None;
        }
        (frame, new_slot)
    } else {
        (old, next)
    };
    let replacement_word = words.get(replacement)?;
    if slot_kind(&replacement_word.normalized) != Some(kind)
        || replacement_word.normalized == words[old].normalized
    {
        return None;
    }
    if kind == AnchorKind::Number
        && [words[old].raw, replacement_word.raw].iter().any(|w| {
            w.chars().any(|c| c.is_ascii_digit()) && !w.chars().all(|c| c.is_ascii_digit())
        })
    {
        return None;
    }
    // Questions/quotes are never a cue boundary for deterministic edits.
    if text[words[old].bytes.end..words[next].bytes.start].contains(['?', '"', '„', '“', '«', '»'])
    {
        return None;
    }
    let prefix = text[..words[remove].bytes.start].trim_end_matches([' ', '\t', '\r']);
    let sentence_start = prefix.is_empty() || prefix.ends_with(['.', '!', '?', '\n']);
    let kept = &words[next];
    let mut leading = kept.raw.to_string();
    if let Some(first) = leading.chars().next() {
        let capital = if sentence_start {
            first.to_uppercase().collect::<String>()
        } else if next != replacement
            || matches!(kind, AnchorKind::RelativeDay | AnchorKind::Number)
        {
            first.to_lowercase().collect()
        } else {
            first.to_string()
        };
        leading.replace_range(..first.len_utf8(), &capital);
    }
    Some((words[remove].bytes.start..kept.bytes.end, leading))
}

/// Return a deterministic result only when every detected span can be repaired.
/// Otherwise the existing LLM path sees the whole original text, so its guard
/// still uses the original span positions. No partial rewrite can be lost.
pub(super) fn repair(text: &str, spans: &[Span]) -> Option<String> {
    if spans.is_empty() {
        return None;
    }
    let words = words(text);
    let mut cuts = spans
        .iter()
        .map(|s| cut(text, &words, s))
        .collect::<Option<Vec<_>>>()?;
    cuts.sort_by_key(|(range, _)| range.start);
    if cuts.windows(2).any(|w| w[0].0.end > w[1].0.start) {
        return None;
    }
    let mut result = text.to_string();
    for (range, replacement) in cuts.into_iter().rev() {
        result.replace_range(range, &replacement);
    }
    guard::check(text, &result, spans).ok()?;
    Some(result)
}

#[cfg(test)]
mod tests {
    use super::super::disfluency::detect;
    use super::*;

    const CASES: &[(&str, &str)] = &[
        ("Ich komme morgen. Ne, übermorgen.", "Ich komme übermorgen."),
        (
            "Ich komme morgen. Korrigiere übermorgen.",
            "Ich komme übermorgen.",
        ),
        (
            "Treffen am Montag beziehungsweise Dienstag.",
            "Treffen am Dienstag.",
        ),
        ("Treffen am Montag bzw. Dienstag.", "Treffen am Dienstag."),
        (
            "Verschieb das in den März. Nein in den April.",
            "Verschieb das in den April.",
        ),
        (
            "Wir treffen uns um drei, nein warte, um vier.",
            "Wir treffen uns um vier.",
        ),
        ("Schick das an Tom. Nein an Tim.", "Schick das an Tim."),
        ("Ich komme morgen übermorgen.", "Ich komme übermorgen."),
        ("See you at five, make that six.", "See you at six."),
        (
            "Treffen am Montag, beziehungsweise Dienstag.",
            "Treffen am Dienstag.",
        ),
        ("Am Montag. Nein am Dienstag!", "Am Dienstag!"),
        ("Morgen. Ne, übermorgen.", "Übermorgen."),
        ("Ich komme morgen. Ne, Übermorgen", "Ich komme übermorgen"),
        (
            "Wir treffen uns um drei, nein warte, um vier Uhr.",
            "Wir treffen uns um vier Uhr.",
        ),
        (
            "Meet at five, make that six. Ship on Monday, no wait, on Tuesday.",
            "Meet at six. Ship on Tuesday.",
        ),
        (
            "Meet in the afternoon at five, make that six.",
            "Meet in the afternoon at six.",
        ),
    ];

    #[test]
    fn slot_repair_table() {
        for &(input, output) in CASES {
            assert_eq!(
                repair(input, &detect(input, Some("de"))).as_deref(),
                Some(output),
                "{input}"
            );
        }
    }

    #[test]
    fn ambiguous_and_complex_repairs_stay_with_the_llm() {
        for input in [
            "Ich komme morgen und übermorgen.",
            "Montag oder Dienstag?",
            "drei, vier Tage",
            "Ne, das passt schon.",
            "Korrigiere bitte den Text.",
            "Ich nehme Tee bzw. Kaffee.",
            "Meet at five tonight, make that six.",
            "Meet at five, make that on Monday.",
            "Meet at 15:30, no wait, at 16:45.",
            "Ich wollte – ich muss jetzt los.",
            "Öhm, ich komme morgen. Ne, übermorgen.",
            "Guten Morgen, nein warte, übermorgen.",
            "Meet at five, no wait, on six.",
            "Ich komme morgen, beziehungsweise erst übermorgen.",
        ] {
            assert_eq!(repair(input, &detect(input, Some("de"))), None, "{input}");
        }
    }
}
