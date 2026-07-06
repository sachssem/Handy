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

/// Insert `correction`, or, if a pair with the same id already exists, bump its
/// `count`/`last_seen` in place (preserving the user's `enabled` choice and
/// refreshing `intended`). Returns the id of the affected entry.
pub fn upsert(corrections: &mut Vec<LearnedCorrection>, correction: LearnedCorrection) -> String {
    let id = correction.id.clone();
    if let Some(existing) = corrections.iter_mut().find(|c| c.id == id) {
        existing.count = existing.count.saturating_add(1);
        existing.last_seen = correction.last_seen;
        existing.intended = correction.intended;
    } else {
        corrections.push(correction);
    }
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
}
