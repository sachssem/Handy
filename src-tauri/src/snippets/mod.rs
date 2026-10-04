//! Snippets (fork feature: voice-control): a spoken trigger phrase expands to
//! user-defined text, e.g. "meine Adresse" → a full postal address.
//!
//! Semantics (precision over recall — a snippet must never fire inside prose):
//! - A trigger fires only as a **delimited phrase**: its words fill a whole
//!   clause: preceded by the text edge, a sentence delimiter or comma, and
//!   followed by the text edge or a sentence delimiter (`. : ! ?`, newline).
//!   A comma after the trigger can introduce a relative clause, so is unsafe.
//!   The whole dictation being the trigger is the common case.
//!   "Schick das an meine Adresse" never fires;
//!   "Hallo Marc, meine Adresse." does.
//! - Matching is case-insensitive on words only, so ASR punctuation inside the
//!   trigger ("Meine, Adresse.") and its casing do not matter.
//! - Punctuation the ASR put around a trigger at a text edge is dropped (the
//!   expansion is inserted verbatim); delimiters between it and other words
//!   stay.
//! - Longest trigger first; disabled snippets never fire.
//!
//! Protection: the expansion must not be rewritten by later stages (text
//! rules, learned corrections). [`shield`] therefore replaces each fired
//! trigger by a private-use placeholder character (no word class, so every
//! later pass leaves it alone) and [`Shielded::restore`] puts the expansion
//! back once those stages ran. The output stages after the transcription
//! (self-correction LLM pass, per-app style) see the restored text and use
//! the dictation-keyed [`take_protection`] snapshot of actual restored spans.

pub(crate) mod commands;

use serde::{Deserialize, Serialize};
use specta::Type;
use std::ops::Range;
use std::sync::{Mutex, OnceLock};

/// A user-defined snippet.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Type)]
pub struct Snippet {
    pub id: String,
    /// Spoken trigger phrase, e.g. `"meine Adresse"`.
    pub trigger: String,
    /// Inserted text (multi-line allowed), inserted verbatim.
    pub expansion: String,
    #[serde(default = "crate::settings::default_true")]
    pub enabled: bool,
}

/// First placeholder code point (Unicode BMP private-use area).
const PLACEHOLDER_BASE: u32 = 0xE000;
/// Placeholders available per text (more fired snippets are left as words).
const MAX_SLOTS: usize = 0x100;
/// Characters that delimit a clause around a trigger.
const SENTENCE_DELIMITERS: &[char] = &['.', ':', '!', '?', '\n'];
/// Completed dictation protections retained while newer dictations finish.
const PROTECTION_KEEP: usize = 8;

/// Actual snippet insertions, never inferred from coincidental expansion text.
#[derive(Debug, Default, Clone)]
pub(crate) struct Protection {
    pub fired: bool,
    text: String,
    spans: Vec<Range<usize>>,
}

impl Protection {
    /// Byte offsets are valid only for the restored text. Upstream post-processing
    /// may replace it; retain the fired flag but never reuse stale offsets.
    pub fn spans_in(&self, text: &str) -> &[Range<usize>] {
        if self.text == text {
            &self.spans
        } else {
            &[]
        }
    }
}

#[derive(Default)]
struct ProtectionSlot(Vec<(u64, Protection)>);

impl ProtectionSlot {
    fn put(&mut self, id: u64, protection: Protection) {
        self.0.retain(|(stored, _)| *stored != id);
        self.0.push((id, protection));
        // Like the context store, retain a small bounded set so a newer
        // dictation cannot overwrite an earlier one still finishing.
        if self.0.len() > PROTECTION_KEEP {
            self.0.remove(0);
        }
    }

    fn take(&mut self, id: Option<u64>) -> Protection {
        self.0
            .iter()
            .position(|(stored, _)| Some(*stored) == id)
            .map(|index| self.0.remove(index).1)
            .unwrap_or_default()
    }
}

static PROTECTION: Mutex<ProtectionSlot> = Mutex::new(ProtectionSlot(Vec::new()));

pub(crate) fn take_protection(id: Option<u64>) -> Protection {
    PROTECTION
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take(id)
}

/// The words of `text`, lowercased: maximal alphanumeric runs.
pub(crate) fn trigger_words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Text with fired snippets replaced by placeholders.
#[derive(Debug, Default)]
pub(crate) struct Shielded {
    pub text: String,
    /// Expansion per placeholder (index = code point − [`PLACEHOLDER_BASE`]).
    slots: Vec<String>,
    dictation_id: Option<u64>,
    published: OnceLock<()>,
}

impl Shielded {
    /// Whether any snippet fired.
    pub fn fired(&self) -> bool {
        !self.slots.is_empty()
    }

    /// Put the expansions back in place of their placeholders.
    pub fn restore(&self, text: &str) -> String {
        let (out, protection) = self.restore_with_protection(text);
        // Production restores learned output first, then intermediate journal
        // stages. Only the first restore supplies offsets for the final output.
        self.published.get_or_init(|| {
            if let Some(id) = self.dictation_id {
                PROTECTION
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .put(id, protection);
            }
        });
        out
    }

    fn restore_with_protection(&self, text: &str) -> (String, Protection) {
        let mut out = String::with_capacity(text.len());
        let mut spans = Vec::new();
        for c in text.chars() {
            match slot_of(c).and_then(|i| self.slots.get(i)) {
                Some(expansion) => {
                    let start = out.len();
                    out.push_str(expansion);
                    spans.push(start..out.len());
                }
                None => out.push(c),
            }
        }
        let protection = Protection {
            fired: self.fired(),
            text: out.clone(),
            spans,
        };
        (out, protection)
    }
}

fn slot_of(c: char) -> Option<usize> {
    let code = c as u32;
    (PLACEHOLDER_BASE..PLACEHOLDER_BASE + MAX_SLOTS as u32)
        .contains(&code)
        .then(|| (code - PLACEHOLDER_BASE) as usize)
}

/// A word of the text with its byte range.
struct Word {
    start: usize,
    end: usize,
    lower: String,
}

fn words_of(text: &str) -> Vec<Word> {
    let mut words = Vec::new();
    let mut start: Option<usize> = None;
    for (i, c) in text.char_indices() {
        match (c.is_alphanumeric(), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                words.push(Word {
                    start: s,
                    end: i,
                    lower: text[s..i].to_lowercase(),
                });
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        words.push(Word {
            start: s,
            end: text.len(),
            lower: text[s..].to_lowercase(),
        });
    }
    words
}

/// The byte range to replace for the first delimited occurrence of
/// `trigger` (lowercased words) in `text`.
fn find_delimited(text: &str, trigger: &[String]) -> Option<(usize, usize)> {
    let words = words_of(text);
    let n = trigger.len();
    if n == 0 || words.len() < n {
        return None;
    }
    for i in 0..=words.len() - n {
        if !words[i..i + n]
            .iter()
            .zip(trigger)
            .all(|(w, t)| &w.lower == t)
        {
            continue;
        }
        let first = &words[i];
        let last = &words[i + n - 1];
        let before = &text[..first.start];
        let after = &text[last.end..];
        let at_start = i == 0;
        let at_end = i + n == words.len();
        let before_sep = if at_start {
            before
        } else {
            &text[words[i - 1].end..first.start]
        };
        let after_sep = if at_end {
            after
        } else {
            &text[last.end..words[i + n].start]
        };
        // A placeholder (another fired snippet) at the text edge is content:
        // the trigger is then bounded by the delimiter test, and nothing
        // beyond the trigger is swallowed.
        let edge_before = at_start && !before.chars().any(|c| slot_of(c).is_some());
        let edge_after = at_end && !after.chars().any(|c| slot_of(c).is_some());
        if !((edge_before || before_sep.contains(SENTENCE_DELIMITERS) || before_sep.contains(','))
            && (edge_after || after_sep.contains(SENTENCE_DELIMITERS)))
        {
            continue;
        }
        // At a text edge the ASR's punctuation around the trigger goes too.
        let start = if edge_before { 0 } else { first.start };
        let end = if edge_after { text.len() } else { last.end };
        return Some((start, end));
    }
    None
}

/// Replace every delimited trigger of an enabled snippet in `text` by a
/// placeholder. See the module docs for the matching rules.
pub(crate) fn shield(text: &str, snippets: &[Snippet]) -> Shielded {
    let mut candidates: Vec<(Vec<String>, &Snippet)> = snippets
        .iter()
        .filter(|s| s.enabled && !s.expansion.is_empty())
        .map(|s| (trigger_words(&s.trigger), s))
        .filter(|(words, _)| !words.is_empty())
        .collect();
    // Longest trigger first, so "meine Adresse privat" beats "meine Adresse".
    candidates.sort_by_key(|(words, _)| std::cmp::Reverse(words.len()));

    let mut shielded = Shielded {
        text: text.to_string(),
        slots: Vec::new(),
        dictation_id: crate::journal::last_dictation_id(),
        published: OnceLock::new(),
    };
    for (words, snippet) in &candidates {
        while shielded.slots.len() < MAX_SLOTS {
            let Some((start, end)) = find_delimited(&shielded.text, words) else {
                break;
            };
            let Some(placeholder) = char::from_u32(PLACEHOLDER_BASE + shielded.slots.len() as u32)
            else {
                break;
            };
            log::debug!("snippets: '{}' fired", snippet.trigger);
            shielded.slots.push(snippet.expansion.clone());
            shielded
                .text
                .replace_range(start..end, &placeholder.to_string());
        }
    }
    shielded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(trigger: &str, expansion: &str) -> Snippet {
        Snippet {
            id: trigger.to_string(),
            trigger: trigger.to_string(),
            expansion: expansion.to_string(),
            enabled: true,
        }
    }

    const ADDRESS: &str = "Marc Sachße\nMusterstraße 1\n12345 Berlin";

    fn expand(text: &str, snippets: &[Snippet]) -> String {
        let shielded = shield(text, snippets);
        shielded.restore(&shielded.text)
    }

    #[test]
    fn whole_dictation_fires_and_drops_asr_punctuation() {
        let snippets = [snippet("meine Adresse", ADDRESS)];
        for input in [
            "Meine Adresse.",
            "meine adresse",
            "Meine, Adresse!",
            " Meine Adresse. ",
        ] {
            assert_eq!(expand(input, &snippets), ADDRESS, "input: {input}");
        }
    }

    #[test]
    fn delimited_phrase_fires_inside_a_dictation() {
        let snippets = [snippet("mein Calendly", "https://calendly.com/marc")];
        assert_eq!(
            expand("Hier ist der Link, mein Calendly.", &snippets),
            "Hier ist der Link, https://calendly.com/marc"
        );
        assert_eq!(
            expand("Mein Calendly. Bis morgen!", &snippets),
            "https://calendly.com/marc. Bis morgen!"
        );
    }

    #[test]
    fn prose_use_does_not_fire() {
        let snippets = [snippet("meine Adresse", ADDRESS)];
        for input in [
            "Schick das an meine Adresse.",
            "Meine Adresse hat sich geändert.",
            "Ist das meine Adresse?",
            "Meine Adresse, die alte, ist ungültig.",
        ] {
            assert_eq!(expand(input, &snippets), input, "input: {input}");
        }
    }

    #[test]
    fn english_relative_clause_does_not_fire() {
        let snippets = [snippet("my address", ADDRESS)];
        let input = "My address, as you know, has changed.";
        assert_eq!(expand(input, &snippets), input);
    }

    #[test]
    fn comma_before_trigger_and_sentence_end_still_fire() {
        let snippets = [snippet("mein Calendly", "https://calendly.com/marc")];
        assert_eq!(
            expand("Hallo Marc, mein Calendly.", &snippets),
            "Hallo Marc, https://calendly.com/marc"
        );
    }

    #[test]
    fn longest_trigger_wins_and_disabled_never_fires() {
        let snippets = [
            snippet("Adresse", "short"),
            snippet("Adresse privat", "long"),
            Snippet {
                enabled: false,
                ..snippet("Signatur", "sig")
            },
        ];
        assert_eq!(expand("Adresse privat.", &snippets), "long");
        assert_eq!(expand("Signatur.", &snippets), "Signatur.");
    }

    #[test]
    fn several_snippets_in_one_dictation() {
        let snippets = [snippet("Adresse", "A"), snippet("Telefon", "T")];
        assert_eq!(expand("Adresse. Telefon.", &snippets), "A. T");
        assert_eq!(expand("Adresse, Telefon.", &snippets), "Adresse, T");
    }

    #[test]
    fn placeholder_survives_text_rules() {
        let snippets = [snippet(
            "mein Calendly",
            "https://calendly.com/marc. Punkt Komma",
        )];
        let mut settings = crate::settings::get_default_settings();
        settings.text_rules_enabled = true;
        let shielded = shield("voice Bindestrich control, mein Calendly.", &snippets);
        assert!(shielded.fired());
        let ruled = crate::text_rules::apply_text_rules(&shielded.text, &settings);
        assert_eq!(
            shielded.restore(&ruled),
            "voice-control, https://calendly.com/marc. Punkt Komma"
        );
    }

    #[test]
    fn protection_tracks_only_actual_insertions_and_byte_offsets() {
        let snippets = [snippet("Adresse", "Welt.")];
        let shielded = shield("Welt. Äh, Adresse.", &snippets);
        let (out, protection) = shielded.restore_with_protection(&shielded.text);
        assert_eq!(out, "Welt. Äh, Welt.");
        assert!(protection.fired);
        assert_eq!(protection.spans_in(&out), std::slice::from_ref(&(11..16)));
        assert!(protection.spans_in("changed output").is_empty());
        let plain = shield("Welt.", &snippets);
        let (_, protection) = plain.restore_with_protection(&plain.text);
        assert!(!protection.fired);
        assert!(protection.spans_in("Welt.").is_empty());
    }

    #[test]
    fn protection_slot_is_dictation_keyed_and_consumed_once() {
        let mut slot = ProtectionSlot::default();
        slot.put(
            7,
            Protection {
                fired: true,
                ..Default::default()
            },
        );
        assert!(!slot.take(Some(8)).fired);
        assert!(!slot.take(None).fired);
        assert!(slot.take(Some(7)).fired);
        assert!(!slot.take(Some(7)).fired);
        slot.put(
            7,
            Protection {
                fired: true,
                ..Default::default()
            },
        );
        slot.put(8, Protection::default());
        assert!(!slot.take(Some(8)).fired);
        assert!(slot.take(Some(7)).fired);
    }

    #[test]
    fn protection_slot_retention_is_bounded() {
        let mut slot = ProtectionSlot::default();
        for id in 1..=9 {
            slot.put(
                id,
                Protection {
                    fired: true,
                    ..Default::default()
                },
            );
        }
        assert_eq!(slot.0.len(), 8);
        assert!(!slot.take(Some(1)).fired);
        assert!(slot.take(Some(2)).fired);
    }
}
