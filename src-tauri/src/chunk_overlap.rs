//! fork(voice-control): long-form chunks decoded with shared audio.
//!
//! Upstream cuts long audio at pauses and decodes every chunk on its own. A
//! model ends each chunk like a sentence and starts the next one cold, so a
//! cut inside a sentence leaves "… wäre ja. die Taster …" in the text, and a
//! cut inside a compound splits the word. Handy's recordings have their
//! silence removed, so a pause inside a sentence looks like one between two.
//!
//! Each chunk is therefore decoded with [`OVERLAP_SAMPLES`] of its neighbours
//! on both sides, and the two texts are joined in the middle of the words
//! they agree on ([`stitch`]): the forced sentence end and the cold start
//! fall into the discarded halves.
//!
//! Qwen3-ASR also writes an occasional chunk without any punctuation or
//! capitals. The same audio decodes the same way, so such a chunk is decoded
//! again with its window moved ([`ATTEMPTS`]).
//!
//! Measured with `handy-bench longform` on 39 cuts of real dictations
//! (2026-11-21, Qwen3-ASR 1.7B Q5_K_M): 3 s per side leaves no cut artefact
//! and matches 5-20 words at every seam; 1 s matches too few, 6 s only costs
//! time. See docs/fork-patches.md.

use std::ops::Range;

use log::{debug, warn};
use transcribe_cpp::Transcript;

use crate::audio_toolkit::chunking::is_unspaced;

const RATE: usize = 16_000;
/// Audio of the neighbouring chunk decoded with a chunk, per side.
const OVERLAP_SAMPLES: usize = 3 * RATE;
/// How far a retry moves a window's edge.
const SHIFT_SAMPLES: usize = RATE * 3 / 2;
/// Audio before and after a chunk that each attempt adds to it. The first is
/// the normal decode; the rest only run for a chunk that came back bare.
const ATTEMPTS: [(usize, usize); 4] = [
    (OVERLAP_SAMPLES, OVERLAP_SAMPLES),
    (OVERLAP_SAMPLES + SHIFT_SAMPLES, OVERLAP_SAMPLES),
    (OVERLAP_SAMPLES, OVERLAP_SAMPLES + SHIFT_SAMPLES),
    (
        OVERLAP_SAMPLES + 2 * SHIFT_SAMPLES,
        OVERLAP_SAMPLES + SHIFT_SAMPLES,
    ),
];
/// The most audio any attempt takes from the chunk before.
const KEEP_SAMPLES: usize = OVERLAP_SAMPLES + 2 * SHIFT_SAMPLES;

/// Whether a bare chunk of this model is worth decoding again. Only where it
/// was measured: a model that never punctuates would retry every chunk.
fn retries_bare(model_id: &str) -> bool {
    model_id.to_ascii_lowercase().contains("qwen3-asr")
}

/// One transcription's chunks, decoded in order with shared audio.
pub struct OverlapDecode {
    retry_bare: bool,
    /// The end of the chunk before.
    before: Vec<f32>,
    /// Samples past its own end the chunk before was decoded with.
    trail: usize,
}

impl OverlapDecode {
    pub fn new(model_id: &str) -> Self {
        Self {
            retry_bare: retries_bare(model_id),
            before: Vec::new(),
            trail: 0,
        }
    }

    /// Decode `chunk` with the audio around it and join it to `previous`, the
    /// chunk before: both texts are cut at their seam, so the chunks' texts
    /// still join with a plain space. `after` is the audio that follows the
    /// chunk, as far as it is known. `run` decodes one window.
    pub fn decode<E>(
        &mut self,
        chunk: Vec<f32>,
        after: &[f32],
        previous: Option<&mut Transcript>,
        mut run: impl FnMut(Vec<f32>) -> Result<Transcript, E>,
    ) -> Result<Transcript, E> {
        // At the recording's edges a moved window is the same window.
        let mut windows: Vec<(usize, usize)> = Vec::new();
        for (earlier, later) in ATTEMPTS {
            let window = (earlier.min(self.before.len()), later.min(after.len()));
            if !windows.contains(&window) {
                windows.push(window);
            }
        }
        let mut decode = |(lead, trail): (usize, usize)| {
            let mut pcm = Vec::with_capacity(lead + chunk.len() + trail);
            pcm.extend_from_slice(&self.before[self.before.len() - lead..]);
            pcm.extend_from_slice(&chunk);
            pcm.extend_from_slice(&after[..trail]);
            run(pcm)
        };
        let (mut lead, mut trail) = windows[0];
        let mut transcript = decode(windows[0])?;
        if self.retry_bare && is_bare(&transcript.text) {
            // If every window comes back bare, the speech may simply have no
            // sentences: the first decode stays.
            for &window in &windows[1..] {
                debug!("Long-form chunk came back without punctuation; decoding it again");
                let again = decode(window)?;
                if !is_bare(&again.text) {
                    (transcript, (lead, trail)) = (again, window);
                    break;
                }
            }
        }

        if let Some(previous) = previous {
            let shared = self.trail + lead;
            let window = lead + chunk.len() + trail;
            trim_seam(&mut previous.text, &mut transcript.text, shared, window);
        }

        self.trail = trail;
        self.before
            .extend_from_slice(&chunk[chunk.len().saturating_sub(KEEP_SAMPLES)..]);
        let excess = self.before.len().saturating_sub(KEEP_SAMPLES);
        self.before.drain(..excess);
        Ok(transcript)
    }
}

/// Cut `first` and `second` at their seam. `second` was decoded from `window`
/// samples, the first `shared` of which `first` heard too.
fn trim_seam(first: &mut String, second: &mut String, shared: usize, window: usize) {
    // A chunk without text (silence) has no seam to find.
    if shared == 0 || first.trim().is_empty() || second.trim().is_empty() {
        return;
    }
    let rate = tokens(second).len() as f64 / window as f64;
    let expected = (rate * shared as f64).round() as usize;
    let seam = stitch(first, second, expected);
    if seam.run == 0 {
        warn!("Long-form chunks share no words at their seam; joined by the expected overlap");
    }
    first.truncate(seam.keep_first);
    *second = second.split_off(seam.skip_second);
}

/// Where two chunk texts that share audio are joined.
#[derive(Debug, PartialEq)]
struct Seam {
    /// Bytes of the first text to keep.
    keep_first: usize,
    /// Bytes of the second text to drop.
    skip_second: usize,
    /// Words in the run both texts agree on; 0 when they share none and the
    /// seam was placed by the expected overlap alone.
    run: usize,
}

/// A word, or one character of a script written without spaces.
struct Token {
    range: Range<usize>,
    /// What has to match: its letters and digits in lowercase. Empty for
    /// punctuation.
    key: String,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut open: Option<usize> = None;
    let close = |open: &mut Option<usize>, end: usize, tokens: &mut Vec<Token>| {
        if let Some(start) = open.take() {
            let key = text[start..end]
                .chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            tokens.push(Token {
                range: start..end,
                key,
            });
        }
    };
    for (at, c) in text.char_indices() {
        if c.is_whitespace() {
            close(&mut open, at, &mut tokens);
        } else if is_unspaced(c) {
            close(&mut open, at, &mut tokens);
            open = Some(at);
            close(&mut open, at + c.len_utf8(), &mut tokens);
        } else if open.is_none() {
            open = Some(at);
        }
    }
    close(&mut open, text.len(), &mut tokens);
    tokens
}

/// A chunk the model wrote (nearly) without punctuation: under one mark per
/// 25 words.
pub fn is_bare(text: &str) -> bool {
    let tokens = tokens(text);
    let marks = tokens
        .iter()
        .filter(|token| {
            text[token.range.clone()]
                .chars()
                .next_back()
                .is_some_and(|c| !c.is_alphanumeric())
        })
        .count();
    tokens.len() >= 20 && marks * 25 < tokens.len()
}

/// Find the seam of `first` and `second`, whose shared audio is about
/// `expected` words long: the middle of the longest word run the end of
/// `first` and the start of `second` agree on.
fn stitch(first: &str, second: &str, expected: usize) -> Seam {
    let a = tokens(first);
    let b = tokens(second);
    // The estimate comes from the speaking rate, so look somewhat further.
    let window = expected + expected / 2 + 4;
    let tail_from = a.len().saturating_sub(window);
    let tail = &a[tail_from..];
    let head = &b[..b.len().min(window)];

    // (score, start in tail, start in head, length) of the best run. `runs`
    // holds the length of the common run ending at each pair, row by row.
    let mut best: Option<(f64, usize, usize, usize)> = None;
    let mut runs = vec![0usize; head.len() + 1];
    for (i, tail_token) in tail.iter().enumerate() {
        let mut row = vec![0usize; head.len() + 1];
        for (j, head_token) in head.iter().enumerate() {
            if tail_token.key.is_empty() || tail_token.key != head_token.key {
                continue;
            }
            let len = runs[j] + 1;
            row[j + 1] = len;
            // Only a whole run is scored.
            let continues = tail.get(i + 1).is_some_and(|next| {
                !next.key.is_empty() && head.get(j + 1).is_some_and(|other| other.key == next.key)
            });
            if continues {
                continue;
            }
            let (start_a, start_b) = (i + 1 - len, j + 1 - len);
            let chars: usize = tail[start_a..=i]
                .iter()
                .map(|token| token.key.chars().count())
                .sum();
            // One short word agrees by chance.
            if len < 2 && chars < 6 {
                continue;
            }
            // A true match lies where the shared audio is: the words from the
            // run to the first text's end plus those before it in the second
            // add up to the shared words. A phrase repeated elsewhere does not.
            let spans = (tail.len() - start_a) + start_b;
            let off = spans.abs_diff(expected) as f64;
            let score = len as f64 + chars as f64 / 100.0 - 0.25 * off;
            if best.is_none_or(|(best, ..)| score > best) {
                best = Some((score, start_a, start_b, len));
            }
        }
        runs = row;
    }

    let (keep, skip, run) = match best {
        Some((_, start_a, start_b, len)) => {
            let half = len / 2;
            (tail_from + start_a + half, start_b + half, len)
        }
        // Nothing agrees: split the expected overlap between the two.
        None => {
            let half = expected / 2;
            (
                a.len().saturating_sub(half),
                (expected - half).min(b.len()),
                0,
            )
        }
    };
    Seam {
        keep_first: if keep == 0 { 0 } else { a[keep - 1].range.end },
        skip_second: b.get(skip).map_or(second.len(), |token| token.range.start),
        run,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio_toolkit::chunking::join_transcripts;

    fn joined(first: &str, second: &str, expected: usize) -> (String, usize) {
        let seam = stitch(first, second, expected);
        let text = join_transcripts(&[&first[..seam.keep_first], &second[seam.skip_second..]]);
        (text, seam.run)
    }

    #[test]
    fn joins_in_the_middle_of_the_shared_run() {
        // The first chunk's forced period and the second's lowercase start
        // both fall into the discarded halves.
        let (text, run) = joined(
            "wir legen die Taster dann auf mehrere Inputs bei.",
            "taster dann auf mehrere Inputs bei den Shellys, ohne Skript.",
            6,
        );
        assert_eq!(
            text,
            "wir legen die Taster dann auf mehrere Inputs bei den Shellys, ohne Skript."
        );
        assert_eq!(run, 6);
    }

    #[test]
    fn prefers_the_run_where_the_overlap_is() {
        // "die Shelly" also occurs earlier in the first text.
        let (text, _) = joined(
            "wenn die Shelly schaltet, dann wartet die Shelly quasi kurz.",
            "Die Shelly quasi kurz, bis der zweite Druck kommt.",
            4,
        );
        assert_eq!(
            text,
            "wenn die Shelly schaltet, dann wartet die Shelly quasi kurz, bis der zweite Druck kommt."
        );
    }

    #[test]
    fn without_a_shared_word_splits_the_expected_overlap() {
        let (text, run) = joined("eins zwei drei vier", "fünf sechs sieben acht", 2);
        assert_eq!(run, 0);
        assert_eq!(text, "eins zwei drei sechs sieben acht");
    }

    #[test]
    fn unspaced_script_is_matched_by_character() {
        let (text, run) = joined("今天天气很好我们去", "很好我们去公园吧。", 5);
        assert_eq!(text, "今天天气很好我们去公园吧。");
        assert_eq!(run, 5);
    }

    #[test]
    fn bare_is_a_long_text_without_marks() {
        let bare = "das ist ein text ohne jedes satzzeichen ".repeat(4);
        assert!(is_bare(&bare));
        assert!(!is_bare("Kurz und ohne Punkt"));
        let punctuated = "Das ist ein Text, der Satzzeichen hat. ".repeat(4);
        assert!(!is_bare(&punctuated));
    }

    fn transcript(text: &str) -> Transcript {
        Transcript {
            text: text.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn each_chunk_is_decoded_with_its_neighbours_audio() {
        let mut decode = OverlapDecode::new("parakeet");
        let chunk = |value: f32| vec![value; 10 * RATE];
        let mut windows: Vec<Vec<f32>> = Vec::new();

        let first = decode
            .decode::<()>(chunk(1.0), &chunk(2.0), None, |pcm| {
                windows.push(pcm);
                Ok(transcript("eins zwei drei vier fünf sechs."))
            })
            .unwrap();
        let mut texts = vec![first];
        let second = decode
            .decode::<()>(chunk(2.0), &[], texts.last_mut(), |pcm| {
                windows.push(pcm);
                Ok(transcript(
                    "drei vier fünf sechs sieben acht neun zehn elf zwölf.",
                ))
            })
            .unwrap();
        texts.push(second);

        // First: the chunk plus 3 s of the next. Second: 3 s of the first,
        // then the chunk.
        assert_eq!(windows[0].len(), 13 * RATE);
        assert_eq!(windows[0][10 * RATE], 2.0);
        assert_eq!(windows[1].len(), 13 * RATE);
        assert_eq!(windows[1][3 * RATE - 1], 1.0);
        assert_eq!(
            join_transcripts(&[&texts[0].text, &texts[1].text]),
            "eins zwei drei vier fünf sechs sieben acht neun zehn elf zwölf."
        );
    }

    #[test]
    fn a_bare_chunk_is_decoded_again_with_a_moved_window() {
        let mut decode = OverlapDecode::new("handy-computer/Qwen3-ASR-1.7B-gguf");
        let bare = "ein text ohne satzzeichen und ohne ende ".repeat(4);
        let mut lengths = Vec::new();
        let result = decode
            .decode::<()>(vec![0.0; 10 * RATE], &vec![0.0; 10 * RATE], None, |pcm| {
                lengths.push(pcm.len());
                Ok(transcript(if lengths.len() == 1 {
                    &bare
                } else {
                    "Jetzt mit Punkt."
                }))
            })
            .unwrap();
        assert_eq!(result.text, "Jetzt mit Punkt.");
        // No audio before the first chunk, so the second attempt is the same
        // window and is skipped; the third reaches 1.5 s further.
        assert_eq!(lengths, [13 * RATE, 13 * RATE + RATE * 3 / 2]);
    }

    #[test]
    fn a_chunk_that_stays_bare_keeps_its_first_decode() {
        let mut decode = OverlapDecode::new("qwen3-asr");
        let bare = "ein text ohne satzzeichen und ohne ende ".repeat(4);
        let mut runs = 0;
        let result = decode
            .decode::<()>(vec![0.0; 10 * RATE], &vec![0.0; 10 * RATE], None, |_| {
                runs += 1;
                Ok(transcript(&format!("{bare}{runs}")))
            })
            .unwrap();
        assert_eq!(runs, 2);
        assert!(result.text.ends_with('1'));
    }
}
