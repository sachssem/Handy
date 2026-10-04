//! Word-level correction differ and the anti-poisoning gate pipeline
//! (fork feature: voice-control).
//!
//! The learner wants **vocabulary, not grammar**: names, jargon and product
//! spellings the recognizer keeps getting wrong. Given the field text captured
//! right after the paste (`base`, with the pasted span located inside it) and a
//! later read of the same field, [`extract_anchored`] diffs only the region the
//! user actually changed, keeps the change runs that fall *inside the pasted
//! span* (edits to surrounding text are not about the transcription), and runs
//! each substitution through an ordered gate chain:
//!
//! | gate                 | rejects                                                   |
//! | -------------------- | --------------------------------------------------------- |
//! | `empty`              | a side with no word content                               |
//! | `phrase_length`      | more than [`MAX_PHRASE_WORDS`] words on a side            |
//! | `case_or_punctuation`| case-only / punctuation-only edits                        |
//! | `number`             | a side without letters (`2024 → 2025`)                    |
//! | `distance`           | relative edit distance above the profile bound            |
//! | `likely_typo`        | small edit from a common word to an unknown, non-distinctive word |
//! | `inflection`         | everyday words, inflection-only ending change (`einen → einem`)|
//! | `common_words`       | both sides only everyday de/en words (`Montag → Sonntag`) |
//! | `phonetic`           | Conservative only: borderline pair that does not sound alike |
//!
//! `inflection` and `common_words` let a pair through when the intended side is
//! *distinctive* even though its words are frequent: a first name (any case)
//! (`Jon → John`, `Ann → Anne`), internal capitals (`iPhone`) or a digit/letter
//! mix (`k8s`). `inflection` also needs both words of a pair to be everyday
//! words, so a surname or jargon fix that merely looks like an ending change
//! (`Schmid → Schmidt`, `Postgre → Postgres`) is learned.
//!
//! A substitution that *reverts* a pair the apply stage fires (`Marc → mark`
//! after `mark → Marc`) skips the gates entirely: it is not learned but undoes
//! the pair (see the store), and that must work for any pair.
//!
//! The differ is pure and table-tested. Its live caller is the post-paste
//! learning session, which only exists on macOS.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use super::lexicon;
use rphonetic::{Cologne, DoubleMetaphone, Encoder};
use serde::{Deserialize, Serialize};
use similar::{ChangeTag, DiffableStr, TextDiff};
use specta::Type;
use std::ops::Range;
use std::time::Duration;

/// A correction candidate that survived the gate pipeline: the recognizer's
/// `misheard` span mapped to the user's `intended` replacement, original casing
/// preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub misheard: String,
    pub intended: String,
    /// The intended span starts a sentence in the edited field, so a leading
    /// capital may be positional rather than part of the word (see the apply
    /// stage's casing rules).
    pub sentence_start: bool,
}

/// How permissive the gates are. Since auto-learned pairs are only
/// *suggestions* until they recur or are confirmed, this tunes the distance
/// bound and the phonetic requirement only; the vocabulary gates
/// (`number`, `likely_typo`, `inflection`, `common_words`) apply at every level.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum Aggressiveness {
    /// Tightest distance bound; borderline pairs must also sound alike.
    #[default]
    Conservative,
    /// Looser distance bound, phonetics advisory only.
    Balanced,
    /// Loosest distance bound, no phonetic requirement.
    Aggressive,
}

/// The concrete gate thresholds derived from an [`Aggressiveness`] level.
#[derive(Debug, Clone, Copy)]
pub struct GateProfile {
    /// Maximum edit distance relative to the longer span.
    max_relative_distance: f64,
    /// Whether a borderline substitution additionally has to sound alike. Never
    /// hard-blocks a clearly-low-distance edit (see [`PHONETIC_FLOOR`]).
    require_phonetic: bool,
}

impl GateProfile {
    /// Conservative ⊂ Balanced ⊂ Aggressive.
    pub fn for_aggressiveness(level: Aggressiveness) -> Self {
        match level {
            Aggressiveness::Conservative => GateProfile {
                max_relative_distance: 0.4,
                require_phonetic: true,
            },
            Aggressiveness::Balanced => GateProfile {
                max_relative_distance: 0.5,
                require_phonetic: false,
            },
            Aggressiveness::Aggressive => GateProfile {
                max_relative_distance: 0.7,
                require_phonetic: false,
            },
        }
    }
}

/// Which phonetic algorithm the borderline gate uses. German gets Kölner
/// Phonetik (umlaut/`ß`-aware); everything else Double Metaphone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhoneticLang {
    German,
    Other,
}

impl PhoneticLang {
    /// Map a (base) ISO language code to the phonetic algorithm.
    pub fn for_code(code: Option<&str>) -> Self {
        match code {
            Some(code) if code.starts_with("de") => PhoneticLang::German,
            _ => PhoneticLang::Other,
        }
    }
}

/// A gate that rejected a substitution, named for the session's debug log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    Empty,
    PhraseLength,
    CaseOrPunctuation,
    Number,
    Distance,
    LikelyTypo,
    Inflection,
    CommonWords,
    Phonetic,
}

impl Gate {
    pub fn name(self) -> &'static str {
        match self {
            Gate::Empty => "empty",
            Gate::PhraseLength => "phrase_length",
            Gate::CaseOrPunctuation => "case_or_punctuation",
            Gate::Number => "number",
            Gate::Distance => "distance",
            Gate::LikelyTypo => "likely_typo",
            Gate::Inflection => "inflection",
            Gate::CommonWords => "common_words",
            Gate::Phonetic => "phonetic",
        }
    }
}

/// Maximum words on either side of a learnable substitution.
const MAX_PHRASE_WORDS: usize = 3;

/// Below this relative edit distance the phonetic gate never rejects — it only
/// arbitrates the borderline band above it, keeping one-char fixes like
/// `Muller → Müller` learnable at every level.
const PHONETIC_FLOOR: f64 = 0.25;

/// Maximum change runs inside the pasted span that still count as independent
/// spot fixes; more is a reformulation and nothing is learned.
const MAX_RUNS: usize = 3;

/// Fields longer than this are not diffed at all (a long document; reading and
/// trimming it every keystroke is not worth a vocabulary hint).
pub(super) const MAX_FIELD_CHARS: usize = 50_000;

/// The changed region (after trimming the shared prefix/suffix) must stay
/// below this to be word-diffed; a larger edit is a rewrite, not a fix.
const MAX_DIFF_CHARS: usize = 5_000;

/// Upper bound for one word diff, so a pathological edit can never stall the
/// session thread.
const DIFF_TIMEOUT: Duration = Duration::from_millis(50);

/// Result of one anchored extraction, with the diagnostics the session logs.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Extraction {
    pub candidates: Vec<Candidate>,
    /// The gate that rejected each gated-out substitution, in document order.
    pub rejected: Vec<Gate>,
    /// Change runs inside the pasted span.
    pub runs: usize,
    /// More than [`MAX_RUNS`] runs: treated as a reformulation.
    pub reformulation: bool,
    /// The field or the changed region exceeded the size caps.
    pub oversized: bool,
}

/// Extract every learnable correction from an ASR `original` and an edited
/// `corrected` form where the whole text is the paste. Thin wrapper over
/// [`extract_anchored`].
#[cfg(test)]
pub fn extract_corrections(
    original: &str,
    corrected: &str,
    profile: &GateProfile,
    lang: PhoneticLang,
) -> Vec<Candidate> {
    extract_anchored(original, 0..original.len(), corrected, profile, lang, &[]).candidates
}

/// Diff a later field read (`current`) against the field as captured after the
/// paste (`base`), considering only edits inside `paste` (a byte range of
/// `base`). Text typed before or after the paste, or edits to it, never feed
/// the learner.
///
/// `applied` lists the lowercased `(misheard, intended)` of every pair the
/// apply stage fires; a substitution reverting one of them bypasses the gates.
pub fn extract_anchored(
    base: &str,
    paste: Range<usize>,
    current: &str,
    profile: &GateProfile,
    lang: PhoneticLang,
    applied: &[(String, String)],
) -> Extraction {
    let mut out = Extraction::default();
    if base.chars().count() > MAX_FIELD_CHARS || current.chars().count() > MAX_FIELD_CHARS {
        out.oversized = true;
        return out;
    }
    let (old_mid, new_mid) = changed_region(base, current);
    if old_mid.is_empty() && new_mid.is_empty() {
        return out;
    }
    if base[old_mid.clone()].chars().count() > MAX_DIFF_CHARS
        || current[new_mid.clone()].chars().count() > MAX_DIFF_CHARS
    {
        out.oversized = true;
        return out;
    }

    let runs: Vec<Run> = change_runs(
        &base[old_mid.clone()],
        &current[new_mid.clone()],
        old_mid.start,
        new_mid.start,
    )
    .into_iter()
    .filter(|run| run.inside(&paste))
    .collect();
    out.runs = runs.len();
    if runs.len() > MAX_RUNS {
        out.reformulation = true;
        return out;
    }
    for run in runs.into_iter().filter(Run::is_substitution) {
        let misheard = run.deleted.trim();
        let (intended, intended_offset) = run.intended_within(&paste);
        let candidate = Candidate {
            misheard: misheard.to_string(),
            intended: intended.to_string(),
            sentence_start: is_sentence_start(current, run.new_start + intended_offset),
        };
        if reverts_applied(&candidate, applied) {
            out.candidates.push(candidate);
            continue;
        }
        match gate(&candidate, profile, lang) {
            Ok(()) => out.candidates.push(candidate),
            Err(gate) => out.rejected.push(gate),
        }
    }
    out
}

/// Byte lengths of the longest shared prefix and non-overlapping suffix,
/// both ending on character boundaries in each string.
pub(super) fn common_affixes(base: &str, current: &str) -> (usize, usize) {
    let prefix = base
        .char_indices()
        .zip(current.chars())
        .find(|((_, a), b)| a != b)
        .map(|((idx, _), _)| idx)
        .unwrap_or_else(|| base.len().min(current.len()));
    let max_suffix = (base.len() - prefix).min(current.len() - prefix);
    let mut suffix = base
        .bytes()
        .rev()
        .zip(current.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    // Keep the suffix on a char boundary in both strings.
    while suffix > 0
        && !(base.is_char_boundary(base.len() - suffix)
            && current.is_char_boundary(current.len() - suffix))
    {
        suffix -= 1;
    }
    (prefix, suffix)
}

/// The byte ranges that differ, widened to whole words so the word diff never
/// sees half a word. Linear in the field length.
fn changed_region(base: &str, current: &str) -> (Range<usize>, Range<usize>) {
    let (mut prefix, mut suffix) = common_affixes(base, current);

    // Widen both ends to whole words: a word touching the changed region is
    // diffed whole, so a joined/split compound (`price power → pricepower`) or
    // a hyphen edit (`Chat-GPT → ChatGPT`) is seen as one substitution.
    while let Some(c) = base[..prefix].chars().next_back() {
        if !c.is_alphanumeric() {
            break;
        }
        prefix -= c.len_utf8();
    }
    while suffix > 0 {
        match base[base.len() - suffix..].chars().next() {
            Some(c) if c.is_alphanumeric() => suffix -= c.len_utf8(),
            _ => break,
        }
    }

    (prefix..base.len() - suffix, prefix..current.len() - suffix)
}

/// Whether byte offset `at` starts a sentence, allowing whitespace and closing
/// quotes/brackets after terminal punctuation. A line break also starts one.
pub(super) fn is_sentence_start(text: &str, at: usize) -> bool {
    let at = at.min(text.len());
    for c in text[..at].chars().rev() {
        match c {
            '.' | '!' | '?' | '\n' | '\r' => return true,
            '"' | '\'' | ')' | ']' | '}' | '«' | '»' | '“' | '”' | '‘' | '’' | '›' => {
                continue
            }
            _ if c.is_whitespace() => continue,
            _ => return false,
        }
    }
    true
}

/// A contiguous change run collapsed from the word diff, with its position in
/// both texts.
struct Run {
    deleted: String,
    inserted: String,
    deleted_has_word: bool,
    inserted_has_word: bool,
    /// Byte range of the deleted tokens in `base` (empty for a pure insert).
    old_start: usize,
    old_end: usize,
    /// Byte offset of the inserted tokens in `current`.
    new_start: usize,
}

impl Run {
    fn new(old_start: usize, new_start: usize) -> Self {
        Run {
            deleted: String::new(),
            inserted: String::new(),
            deleted_has_word: false,
            inserted_has_word: false,
            old_start,
            old_end: old_start,
            new_start,
        }
    }

    fn is_empty(&self) -> bool {
        self.deleted.is_empty() && self.inserted.is_empty()
    }

    fn has_word(&self) -> bool {
        self.deleted_has_word || self.inserted_has_word
    }

    /// A real substitution has words on both sides.
    fn is_substitution(&self) -> bool {
        self.deleted_has_word && self.inserted_has_word
    }

    /// Whether the run edits the pasted span. A substitution/delete must lie
    /// within it; a pure insert counts only strictly inside (typing right
    /// before or after the paste is new text, not a fix).
    fn inside(&self, paste: &Range<usize>) -> bool {
        // Ignore leading/trailing whitespace of the deleted span when checking
        // the bounds, so a fix whose run swallowed the separating space before
        // the paste still counts.
        let lead = self.deleted.len() - self.deleted.trim_start().len();
        let trail = self.deleted.len() - self.deleted.trim_end().len();
        let start = self.old_start + lead;
        let end = (self.old_end - trail).max(start);
        if self.deleted_has_word {
            start >= paste.start && end <= paste.end
        } else {
            start > paste.start && start < paste.end
        }
    }

    /// The intended text of a substitution, trimmed, with its byte offset
    /// within `inserted`. When the run sits at an edge of the paste and the
    /// user typed on past it (`Cubernetes` fixed *and* a sentence appended
    /// after it), the diff has no equal token to split on, so the run's insert
    /// swallows the new text. Keep only as many words as were deleted, taken
    /// from the paste side of the run.
    fn intended_within(&self, paste: &Range<usize>) -> (&str, usize) {
        let lead = self.inserted.len() - self.inserted.trim_start().len();
        let inserted = self.inserted.trim();
        let deleted_words = self.deleted.split_whitespace().count();
        let words: Vec<(usize, &str)> = word_spans(inserted);
        if words.len() <= deleted_words {
            return (inserted, lead);
        }
        let trimmed = self.deleted.trim_end();
        let end = self.old_start + trimmed.len();
        let start = self.old_start + (self.deleted.len() - self.deleted.trim_start().len());
        if end >= paste.end {
            let (last_start, last) = words[deleted_words - 1];
            return (&inserted[..last_start + last.len()], lead);
        }
        if start <= paste.start {
            let (first_start, _) = words[words.len() - deleted_words];
            return (&inserted[first_start..], lead + first_start);
        }
        (inserted, lead)
    }
}

/// Whitespace-separated words of `text` with their byte offsets.
fn word_spans(text: &str) -> Vec<(usize, &str)> {
    let mut spans = Vec::new();
    let mut start = None;
    for (idx, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (false, None) => start = Some(idx),
            (true, Some(s)) => {
                spans.push((s, &text[s..idx]));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        spans.push((s, &text[s..]));
    }
    spans
}

/// Word-diff two (already trimmed) regions and collapse the edit into change
/// runs, each a maximal delete/insert cluster between two `Equal` tokens.
/// `old_base`/`new_base` are the regions' byte offsets in the full texts.
fn change_runs(old: &str, new: &str, old_base: usize, new_base: usize) -> Vec<Run> {
    let old_tokens = field_tokens(old);
    let new_tokens = field_tokens(new);
    let diff = TextDiff::configure()
        .timeout(DIFF_TIMEOUT)
        .diff_slices(&old_tokens, &new_tokens);

    let mut runs = Vec::new();
    let mut old_pos = old_base;
    let mut new_pos = new_base;
    let mut current = Run::new(old_pos, new_pos);

    for change in diff.iter_all_changes() {
        let value = change.value();
        match change.tag() {
            ChangeTag::Equal => {
                if !current.is_empty() {
                    runs.push(std::mem::replace(&mut current, Run::new(0, 0)));
                }
                old_pos += value.len();
                new_pos += value.len();
                current = Run::new(old_pos, new_pos);
            }
            ChangeTag::Delete => {
                current.deleted.push_str(value);
                current.deleted_has_word |= is_word(value);
                old_pos += value.len();
                current.old_end = old_pos;
            }
            ChangeTag::Insert => {
                current.inserted.push_str(value);
                current.inserted_has_word |= is_word(value);
                new_pos += value.len();
            }
        }
    }
    if !current.is_empty() {
        runs.push(current);
    }
    // Whitespace/punctuation churn is invisible to the learner.
    runs.retain(Run::has_word);
    runs
}

/// Unicode words with address separators kept as standalone tokens. Keeping
/// every byte preserves field offsets while isolating `mark` in an e-mail or URL.
fn field_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    for word in text.tokenize_unicode_words() {
        for part in word.split_inclusive(['@', '.', '/']) {
            let (body, separator) = if part.ends_with(['@', '.', '/']) {
                part.split_at(part.len() - 1)
            } else {
                (part, "")
            };
            tokens.extend([body, separator].into_iter().filter(|s| !s.is_empty()));
        }
    }
    tokens
}

/// Whether `candidate` maps an applied pair's output back to its input.
fn reverts_applied(candidate: &Candidate, applied: &[(String, String)]) -> bool {
    let misheard = candidate.misheard.trim().to_lowercase();
    let intended = candidate.intended.trim().to_lowercase();
    applied.iter().any(|(pair_misheard, pair_intended)| {
        *pair_intended == misheard && *pair_misheard == intended
    })
}

/// Run the ordered gates over a raw substitution.
fn gate(candidate: &Candidate, profile: &GateProfile, lang: PhoneticLang) -> Result<(), Gate> {
    let misheard_words = normalized_words(&candidate.misheard);
    let intended_words = normalized_words(&candidate.intended);
    if misheard_words.is_empty() || intended_words.is_empty() {
        return Err(Gate::Empty);
    }
    if misheard_words.len() > MAX_PHRASE_WORDS || intended_words.len() > MAX_PHRASE_WORDS {
        return Err(Gate::PhraseLength);
    }
    let misheard = misheard_words.join(" ");
    let intended = intended_words.join(" ");
    if misheard == intended {
        return Err(Gate::CaseOrPunctuation);
    }
    if !has_letter(&misheard) || !has_letter(&intended) {
        return Err(Gate::Number);
    }
    let distance = strsim::levenshtein(&misheard, &intended);
    let span = misheard.chars().count().max(intended.chars().count());
    let relative = (distance as f64) / (span as f64);
    if relative > profile.max_relative_distance {
        return Err(Gate::Distance);
    }
    // Vocabulary gates: only everyday words aimed at a non-distinctive target
    // are grammar/content swaps.
    if !is_distinctive(&candidate.intended) {
        let misheard_raw = raw_words(&candidate.misheard);
        let intended_raw = raw_words(&candidate.intended);
        if misheard_raw.len() == 1
            && intended_raw.len() == 1
            && is_common_word(misheard_raw[0])
            && !is_common_word(intended_raw[0])
            && !lexicon::is_known(&intended)
            && (distance <= 2 || relative <= 0.34)
        {
            return Err(Gate::LikelyTypo);
        }
        if is_inflection_only(&misheard_raw, &intended_raw) {
            return Err(Gate::Inflection);
        }
        if misheard_raw.iter().all(|w| is_common_word(w))
            && intended_raw.iter().all(|w| is_common_word(w))
        {
            return Err(Gate::CommonWords);
        }
    }
    if profile.require_phonetic
        && relative > PHONETIC_FLOOR
        && !sounds_alike(&misheard_words, &intended_words, lang)
    {
        return Err(Gate::Phonetic);
    }
    Ok(())
}

fn has_letter(text: &str) -> bool {
    text.chars().any(char::is_alphabetic)
}

/// The words of `text` that carry word content, verbatim (punctuation kept), in
/// the same order as [`normalized_words`].
fn raw_words(text: &str) -> Vec<&str> {
    text.split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect()
}

/// Whether a verbatim word is an everyday de/en word. A contraction counts
/// when each of its parts does (`you're` → `you`, `re`), which is how the
/// frequency lists store them.
fn is_common_word(raw: &str) -> bool {
    let normalized = normalize_word(raw);
    if lexicon::is_common(&normalized) {
        return true;
    }
    raw.contains(['\'', '’'])
        && raw
            .split(['\'', '’'])
            .map(normalize_word)
            .filter(|part| !part.is_empty())
            .all(|part| lexicon::is_common(&part))
}

/// Same word count, and every differing word pair is two everyday words that
/// share a stem of at least three characters and differ only by inflectional
/// endings. Arguments are verbatim words ([`raw_words`]).
fn is_inflection_only(misheard: &[&str], intended: &[&str]) -> bool {
    if misheard.len() != intended.len() {
        return false;
    }
    let mut any_diff = false;
    for (a, b) in misheard.iter().zip(intended) {
        let (na, nb) = (normalize_word(a), normalize_word(b));
        if na == nb {
            continue;
        }
        any_diff = true;
        if !is_common_word(a) || !is_common_word(b) || !is_inflection_pair(&na, &nb) {
            return false;
        }
    }
    any_diff
}

fn is_inflection_pair(a: &str, b: &str) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let stem = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    if stem < 3 {
        return false;
    }
    let suffix_a: String = a[stem..].iter().collect();
    let suffix_b: String = b[stem..].iter().collect();
    lexicon::is_inflection_ending(&suffix_a) && lexicon::is_inflection_ending(&suffix_b)
}

/// Whether the verbatim intended span carries a vocabulary signal on its own:
/// a first name (any case), internal capitals / all-caps acronyms (`ChatGPT`,
/// `GPT`) or a digit/letter mix (`k8s`). Case-only edits are gated earlier.
fn is_distinctive(intended: &str) -> bool {
    intended.split_whitespace().any(|word| {
        let letters: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        let mut chars = letters.chars();
        chars.next();
        let inner_upper = chars.any(char::is_uppercase);
        let digits_and_letters =
            letters.chars().any(|c| c.is_ascii_digit()) && letters.chars().any(char::is_alphabetic);
        inner_upper || digits_and_letters || lexicon::is_name(&letters)
    })
}

/// Phonetic equality, word by word (same word count required).
fn sounds_alike(misheard: &[String], intended: &[String], lang: PhoneticLang) -> bool {
    misheard.len() == intended.len()
        && misheard.iter().zip(intended).all(|(a, b)| match lang {
            PhoneticLang::German => Cologne.is_encoded_equals(a, b),
            PhoneticLang::Other => DoubleMetaphone::new(None).is_encoded_equals(a, b),
        })
}

/// Whether a diff token contains any word character.
fn is_word(token: &str) -> bool {
    token.chars().any(char::is_alphanumeric)
}

/// Lowercase, punctuation-stripped word list used both for gate comparisons and
/// by the session's relatedness check. Does *not* fold `ß`/`ss` or diacritics.
pub(crate) fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(normalize_word)
        .filter(|word| !word.is_empty())
        .collect()
}

fn normalize_word(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extract_all(original: &str, corrected: &str) -> Vec<Candidate> {
        extract_corrections(
            original,
            corrected,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
        )
    }

    fn extract_with(
        original: &str,
        corrected: &str,
        level: Aggressiveness,
        lang: PhoneticLang,
    ) -> Option<(String, String)> {
        let mut candidates = extract_corrections(
            original,
            corrected,
            &GateProfile::for_aggressiveness(level),
            lang,
        );
        match candidates.len() {
            1 => {
                let c = candidates.remove(0);
                Some((c.misheard, c.intended))
            }
            _ => None,
        }
    }

    fn extract(original: &str, corrected: &str) -> Option<(String, String)> {
        extract_with(
            original,
            corrected,
            Aggressiveness::Balanced,
            PhoneticLang::Other,
        )
    }

    /// The gate that rejected the single substitution of an edit.
    fn rejected_by(original: &str, corrected: &str) -> Option<Gate> {
        let out = extract_anchored(
            original,
            0..original.len(),
            corrected,
            &GateProfile::for_aggressiveness(Aggressiveness::Aggressive),
            PhoneticLang::Other,
            &[],
        );
        assert!(
            out.candidates.is_empty(),
            "{original} → {corrected} learned"
        );
        out.rejected.first().copied()
    }

    fn pair(misheard: &str, intended: &str) -> Option<(String, String)> {
        Some((misheard.to_string(), intended.to_string()))
    }

    #[test]
    fn clean_name_substitution_is_learned() {
        assert_eq!(
            extract("send it to Jon", "send it to John"),
            pair("Jon", "John")
        );
    }

    #[test]
    fn likely_typos_are_rejected_at_every_level() {
        for (original, corrected) in [
            ("Milch", "Mlch"),
            ("Brot", "Broti"),
            ("Eier", "Eir"),
            ("house", "hosue"),
        ] {
            assert_eq!(rejected_by(original, corrected), Some(Gate::LikelyTypo));
            for level in [
                Aggressiveness::Conservative,
                Aggressiveness::Balanced,
                Aggressiveness::Aggressive,
            ] {
                assert_eq!(
                    extract_with(original, corrected, level, PhoneticLang::Other),
                    None
                );
            }
        }
        assert_eq!(Gate::LikelyTypo.name(), "likely_typo");
    }

    #[test]
    fn shopping_list_typos_do_not_become_suggestions() {
        let base = "Einkaufsliste:\n1. Milch\n2. Eier\n3. Brot";
        let out = extract_anchored(
            base,
            0..base.len(),
            "Einkaufsliste:\n1. Mlch\n2. Eir\n3. Broti",
            &GateProfile::for_aggressiveness(Aggressiveness::Aggressive),
            PhoneticLang::German,
            &[],
        );
        assert!(out.candidates.is_empty());
        assert_eq!(out.rejected, vec![Gate::LikelyTypo; 3]);
        assert!(!out.reformulation);
    }

    #[test]
    fn likely_typo_uses_absolute_or_relative_distance() {
        // Two edits suffice even above 0.34; three edits still count below it.
        assert_eq!(rejected_by("car", "cxy"), Some(Gate::LikelyTypo));
        assert_eq!(
            rejected_by("university", "univorsatyx"),
            Some(Gate::LikelyTypo)
        );
        // Three edits at 0.5 are outside the typo heuristic.
        assert_eq!(extract("house", "haxsez"), pair("house", "haxsez"));
    }

    #[test]
    fn vocabulary_corrections_survive_the_typo_gate() {
        for (misheard, intended) in [
            ("Cubernetes", "Kubernetes"),
            ("Schmid", "Schmidt"),
            ("Chat-GTP", "ChatGPT"),
            ("price power", "pricepower"),
            ("kas", "k8s"),
            ("kbs", "k8s"),
            ("Jon", "John"),
            ("house", "hoZse"),
            ("house", "h0use"),
            ("house", "HOZSE"),
        ] {
            assert_eq!(extract(misheard, intended), pair(misheard, intended));
        }
    }

    #[test]
    fn first_name_spelling_fixes_are_learned_in_any_case() {
        for (misheard, intended) in [("mark", "marc"), ("Mark", "Marc")] {
            for level in [
                Aggressiveness::Conservative,
                Aggressiveness::Balanced,
                Aggressiveness::Aggressive,
            ] {
                assert_eq!(
                    extract_with(misheard, intended, level, PhoneticLang::German),
                    pair(misheard, intended),
                );
            }
        }
        assert_eq!(rejected_by("marc", "Marc"), Some(Gate::CaseOrPunctuation));
    }

    #[test]
    fn field_diff_extracts_name_corrections_inside_emails_and_urls() {
        for (before, after) in [
            ("mark@example.com", "marc@example.com"),
            ("mark.sachsse@example.com", "marc.sachsse@example.com"),
            ("https://example.com/mark", "https://example.com/marc"),
            (
                "https://mark.example.com/path",
                "https://marc.example.com/path",
            ),
        ] {
            let prefix = "Entwurf: ";
            let pasted = format!("Schreib an {before}.");
            let base = format!("{prefix}{pasted} Ende");
            let current = format!("{prefix}Schreib an {after}. Ende");
            let out = extract_anchored(
                &base,
                prefix.len()..prefix.len() + pasted.len(),
                &current,
                &GateProfile::for_aggressiveness(Aggressiveness::Conservative),
                PhoneticLang::German,
                &[],
            );
            assert_eq!(out.candidates.len(), 1, "{before} → {after}: {out:?}");
            assert_eq!(out.candidates[0].misheard, "mark");
            assert_eq!(out.candidates[0].intended, "marc");
            assert!(!out.candidates[0].sentence_start);
            assert!(out.rejected.is_empty());
        }
    }

    #[test]
    fn field_tokens_preserve_bytes_and_split_address_separators() {
        let text = "Schreib an märk.sachsse@example.com / Chat-GTP 😀";
        let tokens = field_tokens(text);
        assert_eq!(tokens.concat(), text);
        assert!(tokens.contains(&"@"));
        assert!(tokens.contains(&"."));
        assert!(tokens.contains(&"/"));
        for word in ["märk", "sachsse", "example", "com"] {
            assert!(tokens.contains(&word), "{word}");
        }
        assert!(tokens.iter().all(|token| !token.is_empty()));
    }

    #[test]
    fn grammar_and_everyday_swaps_are_rejected_at_every_level() {
        // Inflections, numbers, homophones and weekday swaps change grammar
        // or content rather than vocabulary.
        let cases = [
            (
                "ich habe einen Termin",
                "ich habe einem Termin",
                Gate::Inflection,
            ),
            ("mit diesen Leuten", "mit diesem Leuten", Gate::Inflection),
            ("der Termin morgen", "der Termine morgen", Gate::Inflection),
            ("im Jahr 2024", "im Jahr 2025", Gate::Number),
            ("see you Tuesday", "see you Thursday", Gate::CommonWords),
            ("ihr seid da", "ihr seit da", Gate::Inflection),
            ("is that your car", "is that you're car", Gate::Inflection),
            ("bis Montag dann", "bis Sonntag dann", Gate::CommonWords),
        ];
        for (original, corrected, gate) in cases {
            assert_eq!(
                rejected_by(original, corrected),
                Some(gate),
                "{original} → {corrected}"
            );
        }
    }

    #[test]
    fn jargon_and_names_are_learned() {
        assert_eq!(
            extract("deploy it on kas today", "deploy it on k8s today"),
            pair("kas", "k8s")
        );
        assert_eq!(
            extract("the Cubernetes cluster", "the Kubernetes cluster"),
            pair("Cubernetes", "Kubernetes")
        );
        assert_eq!(
            extract("Grüße, Marc Sachse", "Grüße, Marc Sachße"),
            pair("Sachse", "Sachße")
        );
        assert_eq!(
            extract("bei price power arbeiten", "bei pricepower arbeiten"),
            pair("price power", "pricepower")
        );
        // Both spellings are frequent words, but the intended one is a name.
        assert_eq!(
            extract_with(
                "call Steven now",
                "call Stephen now",
                Aggressiveness::Conservative,
                PhoneticLang::Other,
            ),
            pair("Steven", "Stephen")
        );
    }

    #[test]
    fn name_and_jargon_fixes_that_look_like_endings_are_learned() {
        // Surnames, first names and jargon whose fix adds a letter that is
        // also an inflection ending: not grammar, because at least one side
        // is no everyday word or the target is a distinctive name.
        let cases = [
            ("ask Schmid now", "ask Schmidt now", "Schmid", "Schmidt"),
            ("ask Bauman now", "ask Baumann now", "Bauman", "Baumann"),
            ("ask Hofman now", "ask Hofmann now", "Hofman", "Hofmann"),
            ("ask Herman now", "ask Hermann now", "Herman", "Hermann"),
            ("ask Mat now", "ask Matt now", "Mat", "Matt"),
            ("ask Ann now", "ask Anne now", "Ann", "Anne"),
            ("run Postgre now", "run Postgres now", "Postgre", "Postgres"),
        ];
        for (original, corrected, misheard, intended) in cases {
            for level in [
                Aggressiveness::Conservative,
                Aggressiveness::Balanced,
                Aggressiveness::Aggressive,
            ] {
                assert_eq!(
                    extract_with(original, corrected, level, PhoneticLang::Other),
                    pair(misheard, intended),
                    "{original} → {corrected} at {level:?}"
                );
            }
        }
    }

    #[test]
    fn reverting_an_applied_pair_bypasses_the_vocabulary_gates() {
        // `mars → Marc` fired; the user changes it back. Both are everyday
        // words, and `mars` is not distinctive, so a fresh pair would be
        // gated — the revert must still reach the store.
        let applied = vec![("mars".to_string(), "marc".to_string())];
        let base = "ask Marc now";
        let profile = GateProfile::for_aggressiveness(Aggressiveness::Conservative);
        let out = extract_anchored(
            base,
            0..base.len(),
            "ask mars now",
            &profile,
            PhoneticLang::Other,
            &applied,
        );
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(
            (
                out.candidates[0].misheard.as_str(),
                out.candidates[0].intended.as_str()
            ),
            ("Marc", "mars")
        );
        // Without the applied pair the same edit is gated.
        assert!(extract_anchored(
            base,
            0..base.len(),
            "ask mars now",
            &profile,
            PhoneticLang::Other,
            &[],
        )
        .candidates
        .is_empty());
    }

    #[test]
    fn case_and_punctuation_only_edits_are_rejected() {
        assert_eq!(
            rejected_by("i live in münchen", "i live in München"),
            Some(Gate::CaseOrPunctuation)
        );
        assert_eq!(
            rejected_by("ping Chat-GPT now", "ping ChatGPT now"),
            Some(Gate::CaseOrPunctuation)
        );
    }

    #[test]
    fn pure_insert_and_delete_are_not_substitutions() {
        assert_eq!(extract("hello world", "hello there world"), None);
        assert_eq!(extract("hello there world", "hello world"), None);
    }

    #[test]
    fn unrelated_rewrite_fails_distance() {
        assert_eq!(
            rejected_by("meet in Munchen", "meet in Barcelona"),
            Some(Gate::Distance)
        );
    }

    #[test]
    fn hyphenated_replacement_keeps_the_hyphen() {
        assert_eq!(
            extract("we use Wahaflo here", "we use Waha-Flow here"),
            pair("Wahaflo", "Waha-Flow")
        );
    }

    #[test]
    fn hyphenated_misheard_is_captured_whole() {
        assert_eq!(
            extract("ask Chat-GTP about it", "ask ChatGPT about it"),
            pair("Chat-GTP", "ChatGPT")
        );
    }

    #[test]
    fn two_word_fixes_yield_two_candidates() {
        let got: Vec<(String, String)> =
            extract_all("send Kubernetis to Jon", "send Kubernetes to John")
                .into_iter()
                .map(|c| (c.misheard, c.intended))
                .collect();
        assert_eq!(
            got,
            vec![
                ("Kubernetis".to_string(), "Kubernetes".to_string()),
                ("Jon".to_string(), "John".to_string()),
            ]
        );
    }

    #[test]
    fn more_than_max_runs_is_a_reformulation() {
        let base = "x Jon x Steven x Kubernetis x Meier x";
        let out = extract_anchored(
            base,
            0..base.len(),
            "x John x Stephen x Kubernetes x Mayer x",
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert!(out.candidates.is_empty());
        assert!(out.reformulation);
    }

    #[test]
    fn german_umlaut_fix_passes_conservative() {
        assert_eq!(
            extract_with(
                "Ich war bei Muller",
                "Ich war bei Müller",
                Aggressiveness::Conservative,
                PhoneticLang::German,
            ),
            pair("Muller", "Müller")
        );
    }

    #[test]
    fn borderline_unrelated_rejected_under_conservative_only() {
        assert_eq!(
            extract_with(
                "das ist Meier",
                "das ist Meile",
                Aggressiveness::Conservative,
                PhoneticLang::German,
            ),
            None
        );
        assert_eq!(
            extract_with(
                "das ist Meier",
                "das ist Meile",
                Aggressiveness::Balanced,
                PhoneticLang::German,
            ),
            pair("Meier", "Meile")
        );
    }

    #[test]
    fn edits_around_the_paste_are_ignored_and_the_fix_inside_is_found() {
        // Field: "Hi team, " typed before the paste, the paste, and text after.
        let base = "Hi team, deploy on Cubernetes now. Cheers";
        let paste = 9..34; // "deploy on Cubernetes now."
        assert_eq!(&base[paste.clone()], "deploy on Cubernetes now.");
        let current = "Hello team, deploy on Kubernetes now. Cheers, Marc";
        let out = extract_anchored(
            base,
            paste,
            current,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert_eq!(
            out.candidates
                .iter()
                .map(|c| (c.misheard.as_str(), c.intended.as_str()))
                .collect::<Vec<_>>(),
            vec![("Cubernetes", "Kubernetes")]
        );
        // `Hi → Hello` lies outside the paste and is no candidate.
        assert_eq!(out.runs, 1);
    }

    #[test]
    fn fix_with_long_text_appended_after_the_paste() {
        let base = "deploy on Cubernetes";
        let current = "deploy on Kubernetes and then write a long follow-up sentence here";
        let out = extract_anchored(
            base,
            0..base.len(),
            current,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].intended, "Kubernetes");
    }

    #[test]
    fn fix_with_text_typed_before_the_paste() {
        let base = "Cubernetes rocks";
        let current = "Hi there, Kubernetes rocks";
        let out = extract_anchored(
            base,
            0..base.len(),
            current,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert_eq!(out.candidates.len(), 1);
        assert_eq!(out.candidates[0].intended, "Kubernetes");
        assert!(!out.candidates[0].sentence_start);
    }

    #[test]
    fn large_field_is_capped() {
        let filler = "lorem ipsum ".repeat(MAX_FIELD_CHARS / 10);
        let base = format!("{filler}deploy on Cubernetes");
        let current = format!("{filler}deploy on Kubernetes");
        let out = extract_anchored(
            &base,
            filler.len()..base.len(),
            &current,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert!(out.oversized);
        assert!(out.candidates.is_empty());
    }

    #[test]
    fn big_field_with_small_edit_is_diffed_cheaply() {
        // Below the field cap, the shared prefix/suffix trimming keeps the
        // diff region tiny.
        let filler = "lorem ipsum ".repeat(2_000);
        let base = format!("{filler}deploy on Cubernetes. {filler}");
        let start = filler.len();
        let end = start + "deploy on Cubernetes.".len();
        let current = format!("{filler}deploy on Kubernetes. {filler}");
        let out = extract_anchored(
            &base,
            start..end,
            &current,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
            &[],
        );
        assert!(!out.oversized);
        assert_eq!(out.candidates.len(), 1);
    }

    #[test]
    fn sentence_start_is_recorded() {
        let out = extract_corrections(
            "Cubernetes is great. we use cubernetis",
            "Kubernetes is great. we use kubernetes",
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
        );
        assert_eq!(out.len(), 2);
        assert!(out[0].sentence_start);
        assert!(!out[1].sentence_start);
    }

    #[test]
    fn sentence_start_after_closing_quotes_is_recorded() {
        let original = "\"Hallo.\" Klaster";
        let corrected = "\"Hallo.\" Cluster";
        let out = extract_corrections(
            original,
            corrected,
            &GateProfile::for_aggressiveness(Aggressiveness::Balanced),
            PhoneticLang::Other,
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].intended, "Cluster");
        assert!(out[0].sentence_start);
        for prefix in ["\"Hallo.\" ", "„Hallo!“ ", "(Hallo?) ", "Hallo\n"] {
            let text = format!("{prefix}Cluster");
            assert!(is_sentence_start(&text, prefix.len()));
        }
        assert!(!is_sentence_start("\"Hallo\" Cluster", "\"Hallo\" ".len()));
    }

    #[test]
    fn common_affixes_are_non_overlapping_utf8_boundaries() {
        for (base, current, expected) in [
            ("abc", "abc", (3, 0)),
            ("abc", "abcd", (3, 0)),
            ("abc", "xbc", (0, 2)),
            ("", "ä", (0, 0)),
            ("äö", "äü", (2, 0)),
            ("é!", "â!", (0, 1)),
            ("hi Jon 😀", "hi John 😀", (5, 6)),
        ] {
            let (prefix, suffix) = common_affixes(base, current);
            assert_eq!((prefix, suffix), expected, "{base:?} → {current:?}");
            assert!(base.is_char_boundary(prefix));
            assert!(current.is_char_boundary(prefix));
            assert!(base.is_char_boundary(base.len() - suffix));
            assert!(current.is_char_boundary(current.len() - suffix));
            assert!(prefix + suffix <= base.len().min(current.len()));
        }
    }

    #[test]
    fn partial_word_mid_typing_is_not_learned() {
        // Mid-word pause while retyping `John`: `Jon → Jo` is a prefix
        // truncation of an everyday token, rejected by the vocabulary gates.
        assert_eq!(extract("send it to Jon", "send it to Jo"), None);
    }
}
