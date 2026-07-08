//! Persistence model for learned corrections (fork feature: voice-control).
//!
//! Learned pairs live in [`crate::settings::AppSettings::learned_corrections`] —
//! the same JSON-store path proven by `text_rules_custom`: free specta bindings,
//! free hot-path access, no separate database. If the list ever grows large
//! enough to need indexing or analytics we can migrate it to `history.db`;
//! until then a `Vec` in settings is the right amount of machinery.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;

/// How a learned correction entered the dictionary.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionSource {
    /// Extracted automatically from a post-paste edit (Phase B).
    Auto,
    /// Entered by the user in the review UI.
    Manual,
}

/// A single learned correction: the recognizer's `misheard` output mapped to the
/// `intended` text the user actually meant.
///
/// `id` is a stable content hash of the (case-insensitive) pair, so re-learning
/// the same correction upserts the existing entry instead of duplicating it.
#[derive(Serialize, Deserialize, Debug, Clone, Type)]
pub struct LearnedCorrection {
    pub id: String,
    pub misheard: String,
    pub intended: String,
    /// How often the pair has been observed. Manual and first-auto adds start
    /// at 1; re-learning bumps it.
    pub count: u32,
    /// Unix timestamp (seconds) of the last time the pair was seen, matching the
    /// history store's `timestamp` convention.
    pub last_seen: i64,
    pub source: CorrectionSource,
    /// Whether the pair is applied at transcription time. A pair can be disabled
    /// without deleting it.
    pub enabled: bool,
    /// The transcription language this pair was learned for (the code the
    /// session's phonetic gate resolved, e.g. `"de"`). `None` — every manual
    /// add — applies to every language; a `Some` pair is only applied when the
    /// current transcription resolves to the same language.
    #[serde(default)]
    pub lang: Option<String>,
}

impl LearnedCorrection {
    /// Build a correction from a `misheard → intended` pair, stamping `id` and
    /// `last_seen`. The spans are stored trimmed but case-preserving.
    pub fn new(misheard: &str, intended: &str, source: CorrectionSource, last_seen: i64) -> Self {
        let misheard = misheard.trim().to_string();
        let intended = intended.trim().to_string();
        let id = correction_id(&misheard, &intended);
        Self {
            id,
            misheard,
            intended,
            count: 1,
            last_seen,
            source,
            enabled: true,
            lang: None,
        }
    }
}

/// Stable identity for a correction pair: a truncated SHA-256 of the case-folded
/// spans. Case-insensitive so that re-learning the same words with a different
/// capitalization does not fork a second entry.
fn correction_id(misheard: &str, intended: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(misheard.trim().to_lowercase().as_bytes());
    hasher.update([0u8]);
    hasher.update(intended.trim().to_lowercase().as_bytes());
    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Upper bound on stored corrections, so unattended auto-learning cannot grow
/// the list without bound. Far above any realistic hand-curated dictionary; it
/// only bites runaway automatic learning. When a new mishearing would exceed it,
/// [`upsert`] evicts the least valuable entry first (see there).
const MAX_LEARNED_CORRECTIONS: usize = 500;

/// Insert `correction`, keyed on the case-folded `misheard` word alone, so a
/// given mishearing has exactly one entry:
///
/// - same `misheard` **and** same `intended` (identical pair re-learned): bump
///   `count`/`last_seen` in place, preserving the user's `enabled` choice;
/// - same `misheard` but a **different** `intended`: replace the entry — adopt
///   the new `intended`/`id`/`lang`/`source`, reset `count` to 1, re-enable it
///   and refresh `last_seen` (the old mapping is superseded, not kept alongside);
/// - unseen `misheard`: push it, evicting the least valuable existing entry
///   first if the list is already at [`MAX_LEARNED_CORRECTIONS`].
///
/// Returns the id of the affected entry.
pub fn upsert(corrections: &mut Vec<LearnedCorrection>, correction: LearnedCorrection) -> String {
    let key = correction.misheard.trim().to_lowercase();
    if let Some(existing) = corrections
        .iter_mut()
        .find(|c| c.misheard.trim().to_lowercase() == key)
    {
        if existing.id == correction.id {
            // Identical pair seen again — bump, keep the user's tweaks.
            existing.count = existing.count.saturating_add(1);
            existing.last_seen = correction.last_seen;
        } else {
            // Same mishearing, new target — the old mapping is replaced.
            existing.id = correction.id;
            existing.intended = correction.intended;
            existing.count = 1;
            existing.last_seen = correction.last_seen;
            existing.source = correction.source;
            existing.enabled = true;
            existing.lang = correction.lang;
        }
        return existing.id.clone();
    }
    // Cap the list before pushing a genuinely new mishearing: when full, evict
    // the least valuable entry — lowest hit `count`, ties broken by oldest
    // `last_seen` — an LRU-ish policy that sheds rarely-used, stale pairs first.
    if corrections.len() >= MAX_LEARNED_CORRECTIONS {
        if let Some(evict) = corrections
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                a.count
                    .cmp(&b.count)
                    .then_with(|| a.last_seen.cmp(&b.last_seen))
            })
            .map(|(idx, _)| idx)
        {
            corrections.remove(evict);
        }
    }
    let id = correction.id.clone();
    corrections.push(correction);
    id
}

/// Remove the correction with `id`. Returns whether an entry was removed.
pub fn remove(corrections: &mut Vec<LearnedCorrection>, id: &str) -> bool {
    let before = corrections.len();
    corrections.retain(|c| c.id != id);
    corrections.len() != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_is_stable_and_case_insensitive() {
        let a = LearnedCorrection::new("Munchen", "München", CorrectionSource::Auto, 0);
        let b = LearnedCorrection::new("munchen", "münchen", CorrectionSource::Manual, 10);
        // Same words, different casing → same identity.
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn upsert_bumps_existing_instead_of_duplicating() {
        let mut list = Vec::new();
        upsert(
            &mut list,
            LearnedCorrection::new("Munchen", "München", CorrectionSource::Manual, 1),
        );
        upsert(
            &mut list,
            LearnedCorrection::new("Munchen", "München", CorrectionSource::Auto, 2),
        );
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].count, 2);
        assert_eq!(list[0].last_seen, 2);
    }

    #[test]
    fn upsert_replaces_intended_for_same_misheard() {
        let mut list = Vec::new();
        let first = LearnedCorrection::new("Jon", "John", CorrectionSource::Auto, 1);
        upsert(&mut list, first);
        // Learn a different target for the same mishearing: replaces, not adds.
        let second = LearnedCorrection::new("jon", "Jonas", CorrectionSource::Manual, 5);
        let second_id = second.id.clone();
        let returned = upsert(&mut list, second);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].intended, "Jonas");
        assert_eq!(list[0].count, 1, "count resets when the mapping changes");
        assert_eq!(list[0].last_seen, 5);
        assert_eq!(list[0].source, CorrectionSource::Manual);
        assert!(list[0].enabled);
        // The id follows the new pair, and is what upsert reports back.
        assert_eq!(list[0].id, second_id);
        assert_eq!(returned, second_id);
    }

    #[test]
    fn remove_by_id() {
        let mut list = vec![LearnedCorrection::new(
            "Munchen",
            "München",
            CorrectionSource::Manual,
            0,
        )];
        let id = list[0].id.clone();
        assert!(remove(&mut list, &id));
        assert!(list.is_empty());
        assert!(!remove(&mut list, &id));
    }

    #[test]
    fn upsert_caps_list_and_evicts_least_valuable() {
        let mut list = Vec::new();
        // Fill to the cap with distinct mishearings. Give the first entry the
        // lowest count and oldest timestamp so it is the eviction target.
        for i in 0..MAX_LEARNED_CORRECTIONS {
            let mut entry = LearnedCorrection::new(
                &format!("word{i}"),
                &format!("fix{i}"),
                CorrectionSource::Auto,
                i as i64,
            );
            // Everyone but the first has a higher hit count.
            entry.count = if i == 0 { 1 } else { 5 };
            upsert(&mut list, entry);
        }
        assert_eq!(list.len(), MAX_LEARNED_CORRECTIONS);
        let evicted_id = list[0].id.clone();

        // A new mishearing must evict the lowest-count/oldest entry, not grow.
        let newcomer = LearnedCorrection::new("brandnew", "shiny", CorrectionSource::Auto, 999);
        let newcomer_id = newcomer.id.clone();
        upsert(&mut list, newcomer);

        assert_eq!(list.len(), MAX_LEARNED_CORRECTIONS);
        assert!(!list.iter().any(|c| c.id == evicted_id));
        assert!(list.iter().any(|c| c.id == newcomer_id));
    }

    #[test]
    fn upsert_at_cap_does_not_evict_when_bumping_existing() {
        let mut list = Vec::new();
        for i in 0..MAX_LEARNED_CORRECTIONS {
            upsert(
                &mut list,
                LearnedCorrection::new(
                    &format!("word{i}"),
                    &format!("fix{i}"),
                    CorrectionSource::Auto,
                    i as i64,
                ),
            );
        }
        // Re-learning an existing pair bumps in place — no eviction, no growth.
        upsert(
            &mut list,
            LearnedCorrection::new("word0", "fix0", CorrectionSource::Auto, 1_000),
        );
        assert_eq!(list.len(), MAX_LEARNED_CORRECTIONS);
        assert!(list.iter().any(|c| c.misheard == "word0"));
    }
}
