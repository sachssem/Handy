//! Learned-correction store and lifecycle (fork feature: voice-control).
//!
//! ## Lifecycle (Wispr-Flow style)
//!
//! - An auto-learned pair enters as [`CorrectionStatus::Suggested`]: stored and
//!   shown, **not applied**.
//! - Observing the same pair again ([`PROMOTE_AT`] observations) or the user
//!   accepting it promotes it to [`CorrectionStatus::Active`]; only active,
//!   enabled pairs are applied at transcription time.
//! - Undo (toast) / reject (UI) removes an auto pair **and blocks it**: a
//!   blocked pair is never learned again (until the user unblocks it or adds it
//!   by hand).
//! - Reverting an applied auto pair by hand (`John → Jon` after `Jon → John`
//!   fired) is detected as its inverse: the original pair is removed and
//!   blocked, and the inverse is *not* learned.
//! - Manual entries belong to the user: auto observations never modify them.
//!   Neither do they replace an auto pair the user disabled: the disable is a
//!   decision about that misheard word.
//!
//! ## Persistence
//!
//! The list lives under its own key ([`STORE_KEY`]) in the settings store file,
//! **not** inside `AppSettings`: every settings command does a whole-object
//! read-modify-write, so a learning session committing while the UI toggles a
//! setting would clobber one or the other. Here every mutation goes through
//! [`update`], which serialises on one mutex, writes only this key, and keeps
//! the in-memory snapshot the apply stage reads on the hot path.

use super::differ::Candidate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use specta::Type;
use std::sync::{Arc, Mutex};

/// How a learned correction entered the dictionary.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionSource {
    /// Extracted automatically from a post-paste edit.
    Auto,
    /// Entered by the user in the review UI.
    Manual,
}

/// Where a pair is in its lifecycle. Only `Active` pairs are applied.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type, Default)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionStatus {
    /// Auto-learned once; awaiting a second observation or user confirmation.
    Suggested,
    /// Applied at transcription time (when `enabled`). The serde default, so
    /// entries persisted before the lifecycle existed load as active and are
    /// then re-classified by [`LearnedCorrections::from_legacy`].
    #[default]
    Active,
}

/// A single learned correction: the recognizer's `misheard` output mapped to the
/// `intended` text the user actually meant.
///
/// `id` is a stable content hash of the (case-insensitive) pair.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Type)]
pub struct LearnedCorrection {
    pub id: String,
    pub misheard: String,
    pub intended: String,
    /// How often the pair has been observed (manual adds start at 1).
    pub count: u32,
    /// Unix timestamp (seconds) of the last observation.
    pub last_seen: i64,
    pub source: CorrectionSource,
    #[serde(default)]
    pub status: CorrectionStatus,
    /// User toggle for an active pair: disabled pairs are kept but not applied.
    pub enabled: bool,
    /// Base ISO code of the transcription language the pair was learned in
    /// (e.g. `"de"`). `None` (every manual add) applies to every language.
    #[serde(default)]
    pub lang: Option<String>,
    /// The intended text was captured at a sentence start, so its leading
    /// capital may be positional; the apply stage then follows the
    /// transcript's casing mid-sentence.
    #[serde(default)]
    pub sentence_start: bool,
}

impl LearnedCorrection {
    /// Build an active, enabled correction from a pair, stamping `id` and
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
            status: CorrectionStatus::Active,
            enabled: true,
            lang: None,
            sentence_start: false,
        }
    }

    /// Whether the apply stage uses this pair.
    pub fn is_applied(&self) -> bool {
        self.status == CorrectionStatus::Active && self.enabled
    }
}

/// A pair the user rejected; never learned again automatically.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Type)]
pub struct BlockedCorrection {
    /// Same content hash as [`LearnedCorrection::id`] for this pair.
    pub id: String,
    pub misheard: String,
    pub intended: String,
    /// Unix timestamp (seconds) of the block.
    pub blocked_at: i64,
}

/// The whole persisted state: the learned list plus the block list.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Type)]
pub struct LearnedCorrections {
    pub corrections: Vec<LearnedCorrection>,
    pub blocked: Vec<BlockedCorrection>,
}

/// Stable identity for a correction pair: a truncated SHA-256 of the case-folded
/// spans.
pub(crate) fn correction_id(misheard: &str, intended: &str) -> String {
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

fn key(text: &str) -> String {
    text.trim().to_lowercase()
}

/// Observations of the same auto pair needed before it becomes active.
const PROMOTE_AT: u32 = 2;

/// Upper bound on stored corrections. When full, the least valuable auto pair
/// (suggestions first, then lowest count, then oldest) is evicted; manual
/// entries are never evicted.
const MAX_LEARNED_CORRECTIONS: usize = 500;

/// Upper bound on the block list; the oldest blocks fall off first.
const MAX_BLOCKED: usize = 1_000;

/// What one auto observation did to the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Observation {
    /// A new suggestion was stored (or replaced an older auto mapping of the
    /// same misheard text).
    Suggested(String),
    /// A suggestion reached [`PROMOTE_AT`] and is now active.
    Promoted(String),
    /// An already active auto pair was seen again (count bumped).
    Reobserved(String),
    /// The pair is on the block list; nothing stored.
    Blocked,
    /// A manual entry already owns this misheard text; left untouched.
    ManualKept,
    /// The candidate reverts an auto pair: that pair was removed and blocked.
    InverseReverted(String),
    /// The candidate reverts a manual pair: the manual pair is kept, the
    /// inverse is not learned.
    InverseOfManual,
    /// The list is full of manual entries; nothing stored.
    Full,
    /// The user disabled the auto pair for this misheard text; a new target
    /// does not replace it.
    DisabledKept,
}

impl Observation {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Suggested(_) => "suggested",
            Self::Promoted(_) => "promoted",
            Self::Reobserved(_) => "re-observed",
            Self::InverseReverted(_) => "reverted+blocked",
            Self::Blocked => "blocked",
            Self::ManualKept => "manual kept",
            Self::InverseOfManual => "inverse of manual",
            Self::Full => "store full",
            Self::DisabledKept => "disabled kept",
        }
    }

    pub fn mutates_store(&self) -> bool {
        matches!(
            self,
            Self::Suggested(_) | Self::Promoted(_) | Self::Reobserved(_) | Self::InverseReverted(_)
        )
    }
}

impl LearnedCorrections {
    /// Re-classify entries persisted before the suggestion lifecycle existed:
    /// manual pairs and auto pairs seen at least [`PROMOTE_AT`] times stay
    /// active, single-observation auto pairs (the over-learned bulk) become
    /// suggestions. Legacy `lang` tags (once the UI locale, e.g. `en-DE`) are
    /// reduced to their base code.
    pub fn from_legacy(legacy: Vec<LearnedCorrection>) -> Self {
        let corrections = legacy
            .into_iter()
            .map(|mut entry| {
                entry.status = match entry.source {
                    CorrectionSource::Manual => CorrectionStatus::Active,
                    CorrectionSource::Auto if entry.count >= PROMOTE_AT => CorrectionStatus::Active,
                    CorrectionSource::Auto => CorrectionStatus::Suggested,
                };
                entry.lang = entry.lang.as_deref().map(super::base_language);
                entry
            })
            .collect();
        Self {
            corrections,
            blocked: Vec::new(),
        }
    }

    pub fn is_blocked(&self, misheard: &str, intended: &str) -> bool {
        let id = correction_id(misheard, intended);
        self.blocked.iter().any(|b| b.id == id)
    }

    fn block(&mut self, misheard: &str, intended: &str, now: i64) {
        let id = correction_id(misheard, intended);
        if self.blocked.iter().any(|b| b.id == id) {
            return;
        }
        if self.blocked.len() >= MAX_BLOCKED {
            if let Some(oldest) = self
                .blocked
                .iter()
                .enumerate()
                .min_by_key(|(_, b)| b.blocked_at)
                .map(|(idx, _)| idx)
            {
                self.blocked.remove(oldest);
            }
        }
        self.blocked.push(BlockedCorrection {
            id,
            misheard: misheard.trim().to_string(),
            intended: intended.trim().to_string(),
            blocked_at: now,
        });
    }

    /// Record one auto-learned candidate. See the module docs for the rules.
    pub fn observe(&mut self, candidate: &Candidate, lang: Option<&str>, now: i64) -> Observation {
        let misheard_key = key(&candidate.misheard);
        let intended_key = key(&candidate.intended);

        if self.is_blocked(&candidate.misheard, &candidate.intended) {
            return Observation::Blocked;
        }

        // The user reverted a pair we know: `candidate` maps what we would
        // have produced back to what the recognizer said.
        if let Some(idx) = self
            .corrections
            .iter()
            .position(|c| key(&c.misheard) == intended_key && key(&c.intended) == misheard_key)
        {
            if self.corrections[idx].source == CorrectionSource::Manual {
                return Observation::InverseOfManual;
            }
            let removed = self.corrections.remove(idx);
            self.block(&removed.misheard, &removed.intended, now);
            return Observation::InverseReverted(removed.id);
        }

        let mut entry = LearnedCorrection::new(
            &candidate.misheard,
            &candidate.intended,
            CorrectionSource::Auto,
            now,
        );
        entry.status = CorrectionStatus::Suggested;
        entry.lang = lang.map(str::to_string);
        entry.sentence_start = candidate.sentence_start;

        if let Some(existing) = self
            .corrections
            .iter_mut()
            .find(|c| key(&c.misheard) == misheard_key)
        {
            if existing.source == CorrectionSource::Manual {
                return Observation::ManualKept;
            }
            if existing.id == entry.id {
                existing.count = existing.count.saturating_add(1);
                existing.last_seen = now;
                if existing.status == CorrectionStatus::Suggested && existing.count >= PROMOTE_AT {
                    existing.status = CorrectionStatus::Active;
                    existing.enabled = true;
                    return Observation::Promoted(existing.id.clone());
                }
                return Observation::Reobserved(existing.id.clone());
            }
            // The user switched this mapping off: respect that rather than
            // sneaking a new target in under the same misheard word.
            if !existing.enabled {
                return Observation::DisabledKept;
            }
            // Same mishearing, new target: the old auto mapping is superseded
            // and the new one starts over as a suggestion.
            let id = entry.id.clone();
            *existing = entry;
            return Observation::Suggested(id);
        }

        if self.corrections.len() >= MAX_LEARNED_CORRECTIONS && !self.evict_one() {
            return Observation::Full;
        }
        let id = entry.id.clone();
        self.corrections.push(entry);
        Observation::Suggested(id)
    }

    /// Evict the least valuable auto pair. `false` when only manual pairs
    /// remain.
    fn evict_one(&mut self) -> bool {
        let victim = self
            .corrections
            .iter()
            .enumerate()
            .filter(|(_, c)| c.source == CorrectionSource::Auto)
            .min_by(|(_, a), (_, b)| {
                // Suggestions (false < true for "is active") go first.
                (a.status == CorrectionStatus::Active)
                    .cmp(&(b.status == CorrectionStatus::Active))
                    .then_with(|| a.count.cmp(&b.count))
                    .then_with(|| a.last_seen.cmp(&b.last_seen))
            })
            .map(|(idx, _)| idx);
        match victim {
            Some(idx) => {
                self.corrections.remove(idx);
                true
            }
            None => false,
        }
    }

    /// Add (or edit) a pair by hand. A manual pair is active immediately,
    /// replaces any entry for the same misheard text, and lifts a block on the
    /// same pair (an explicit add outranks an earlier undo).
    pub fn add_manual(&mut self, misheard: &str, intended: &str, now: i64) -> LearnedCorrection {
        let entry = LearnedCorrection::new(misheard, intended, CorrectionSource::Manual, now);
        self.blocked.retain(|b| b.id != entry.id);
        let misheard_key = key(&entry.misheard);
        match self
            .corrections
            .iter_mut()
            .find(|c| key(&c.misheard) == misheard_key)
        {
            Some(existing) => *existing = entry.clone(),
            None => {
                if self.corrections.len() >= MAX_LEARNED_CORRECTIONS {
                    self.evict_one();
                }
                self.corrections.push(entry.clone());
            }
        }
        entry
    }

    /// Promote a suggestion to active (user confirmation). `false` for an
    /// unknown id.
    pub fn accept(&mut self, id: &str) -> bool {
        match self.corrections.iter_mut().find(|c| c.id == id) {
            Some(entry) => {
                entry.status = CorrectionStatus::Active;
                entry.enabled = true;
                true
            }
            None => false,
        }
    }

    /// Undo / reject: remove every listed pair; auto pairs are also blocked so
    /// they are never learned again. Returns how many entries were removed.
    pub fn reject(&mut self, ids: &[String], now: i64) -> usize {
        let mut removed = 0;
        for id in ids {
            if let Some(idx) = self.corrections.iter().position(|c| &c.id == id) {
                let entry = self.corrections.remove(idx);
                if entry.source == CorrectionSource::Auto {
                    self.block(&entry.misheard, &entry.intended, now);
                }
                removed += 1;
            }
        }
        removed
    }

    /// Delete a pair without blocking it.
    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.corrections.len();
        self.corrections.retain(|c| c.id != id);
        self.corrections.len() != before
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> bool {
        match self.corrections.iter_mut().find(|c| c.id == id) {
            Some(entry) => {
                entry.enabled = enabled;
                true
            }
            None => false,
        }
    }

    pub fn unblock(&mut self, id: &str) -> bool {
        let before = self.blocked.len();
        self.blocked.retain(|b| b.id != id);
        self.blocked.len() != before
    }

    /// Lowercased `(misheard, intended)` of every pair the apply stage fires,
    /// so the learner can recognise a revert of one before its gates run.
    pub fn applied_pairs(&self) -> Vec<(String, String)> {
        self.corrections
            .iter()
            .filter(|c| c.is_applied())
            .map(|c| (key(&c.misheard), key(&c.intended)))
            .collect()
    }

    /// Merge the legacy `AppSettings` list once. After the first migration
    /// (`already_migrated`), a legacy list is stale — e.g. written back by a
    /// whole-settings write holding an old copy — and is ignored, so pairs the
    /// user deleted since never come back. Returns how many pairs were added.
    fn merge_legacy(&mut self, legacy: Vec<LearnedCorrection>, already_migrated: bool) -> usize {
        if already_migrated {
            return 0;
        }
        let migrated = LearnedCorrections::from_legacy(legacy);
        let mut added = 0;
        for entry in migrated.corrections {
            if !self.corrections.iter().any(|c| c.id == entry.id) {
                self.corrections.push(entry);
                added += 1;
            }
        }
        added
    }
}

// --- persistence -------------------------------------------------------------

/// Key of the learned-corrections state inside the settings store file.
const STORE_KEY: &str = "learned_corrections";

/// Set (in the same store write as the migrated list) once the legacy
/// `AppSettings` list was migrated; a legacy list seen afterwards is dropped.
const MIGRATED_KEY: &str = "learned_corrections_migrated";

/// The loaded state; `None` until [`init`]/first [`update`]. The same mutex
/// serialises every read-modify-write.
static STATE: Mutex<Option<Arc<LearnedCorrections>>> = Mutex::new(None);

/// The current state for read-only use (the apply stage). Empty before the
/// store was loaded (e.g. the headless bench harness).
pub fn snapshot() -> Arc<LearnedCorrections> {
    current(&STATE)
}

/// The state behind `slot`, recovering from a poisoned mutex like [`update`]
/// does (the data is replaced whole, never left half-written).
fn current(slot: &Mutex<Option<Arc<LearnedCorrections>>>) -> Arc<LearnedCorrections> {
    slot.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_default()
}

pub(super) fn store(app: &tauri::AppHandle) -> Option<Arc<tauri_plugin_store::Store<tauri::Wry>>> {
    use tauri_plugin_store::StoreExt;
    match app.store(crate::portable::store_path(
        crate::settings::SETTINGS_STORE_PATH,
    )) {
        Ok(store) => Some(store),
        Err(err) => {
            log::error!("learned corrections: store unavailable: {}", err);
            None
        }
    }
}

/// Load the persisted state, migrating the legacy `AppSettings`
/// `learned_corrections` list on first run.
fn load(app: &tauri::AppHandle) -> LearnedCorrections {
    let Some(store) = store(app) else {
        return LearnedCorrections::default();
    };
    let mut data = match store.get(STORE_KEY) {
        Some(value) => serde_json::from_value::<LearnedCorrections>(value).unwrap_or_else(|err| {
            log::warn!("learned corrections: unreadable store entry ({err}); starting empty");
            LearnedCorrections::default()
        }),
        None => LearnedCorrections::default(),
    };

    let already_migrated = store
        .get(MIGRATED_KEY)
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    let mut settings = crate::settings::get_settings(app);
    if !settings.learned_corrections.is_empty() {
        let legacy = std::mem::take(&mut settings.learned_corrections);
        let count = legacy.len();
        let added = data.merge_legacy(legacy, already_migrated);
        if already_migrated {
            log::info!(
                "learned corrections: dropped a stale legacy list ({count} pair(s)) from app settings"
            );
        } else {
            log::info!("learned corrections: migrated {added} legacy pair(s) out of app settings");
        }
        // List and marker land in one store write, before the legacy key is
        // cleared: a crash in between re-runs nothing.
        persist(app, &data);
        store.set(MIGRATED_KEY, true);
        crate::settings::write_settings(app, settings);
    } else if !already_migrated {
        store.set(MIGRATED_KEY, true);
    }
    data
}

fn persist(app: &tauri::AppHandle, data: &LearnedCorrections) {
    if let Some(store) = store(app) {
        match serde_json::to_value(data) {
            Ok(value) => store.set(STORE_KEY, value),
            Err(err) => log::error!("learned corrections: serialize failed: {}", err),
        }
    }
}

/// Load the state into memory (startup). Idempotent.
pub fn init(app: &tauri::AppHandle) {
    let mut state = STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if state.is_none() {
        *state = Some(Arc::new(load(app)));
    }
}

/// The current state, loading it first if needed.
pub fn get(app: &tauri::AppHandle) -> Arc<LearnedCorrections> {
    init(app);
    snapshot()
}

/// Atomically mutate the state: serialised with every other mutation,
/// persisted under [`STORE_KEY`] only, mirrored into the in-memory snapshot,
/// and announced with `LearnedCorrectionsChanged` when `f` reports a change
/// (its second return value).
pub fn update<R>(
    app: &tauri::AppHandle,
    f: impl FnOnce(&mut LearnedCorrections) -> (R, bool),
) -> R {
    let (result, changed) = {
        let mut state = STATE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut data = match state.as_ref() {
            Some(current) => (**current).clone(),
            None => load(app),
        };
        let (result, changed) = f(&mut data);
        if changed {
            persist(app, &data);
        }
        *state = Some(Arc::new(data));
        (result, changed)
    };
    if changed {
        use tauri_specta::Event;
        if let Err(err) = (super::LearnedCorrectionsChanged {}).emit(app) {
            log::error!("Failed to emit learned-corrections-changed event: {}", err);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(misheard: &str, intended: &str) -> Candidate {
        Candidate {
            misheard: misheard.into(),
            intended: intended.into(),
            sentence_start: false,
        }
    }

    fn find<'a>(data: &'a LearnedCorrections, misheard: &str) -> &'a LearnedCorrection {
        data.corrections
            .iter()
            .find(|c| c.misheard == misheard)
            .expect("entry present")
    }

    #[test]
    fn id_is_stable_and_case_insensitive() {
        let a = LearnedCorrection::new("Munchen", "München", CorrectionSource::Auto, 0);
        let b = LearnedCorrection::new("munchen", "münchen", CorrectionSource::Manual, 10);
        assert_eq!(a.id, b.id);
    }

    #[test]
    fn first_observation_is_a_suggestion_second_promotes() {
        let mut data = LearnedCorrections::default();
        let first = data.observe(&candidate("Cubernetes", "Kubernetes"), Some("de"), 1);
        assert!(matches!(first, Observation::Suggested(_)));
        let entry = find(&data, "Cubernetes");
        assert_eq!(entry.status, CorrectionStatus::Suggested);
        assert!(!entry.is_applied());
        assert_eq!(entry.lang.as_deref(), Some("de"));

        let second = data.observe(&candidate("cubernetes", "Kubernetes"), Some("de"), 2);
        assert!(matches!(second, Observation::Promoted(_)));
        let entry = find(&data, "Cubernetes");
        assert_eq!(entry.count, 2);
        assert!(entry.is_applied());

        let third = data.observe(&candidate("Cubernetes", "Kubernetes"), Some("de"), 3);
        assert!(matches!(third, Observation::Reobserved(_)));
        assert_eq!(find(&data, "Cubernetes").count, 3);
    }

    #[test]
    fn accept_activates_a_suggestion() {
        let mut data = LearnedCorrections::default();
        let Observation::Suggested(id) = data.observe(&candidate("Jon", "John"), None, 1) else {
            panic!("expected a suggestion");
        };
        assert!(data.accept(&id));
        assert!(find(&data, "Jon").is_applied());
    }

    #[test]
    fn inverse_revert_removes_and_blocks_the_auto_pair() {
        let mut data = LearnedCorrections::default();
        data.observe(&candidate("Jon", "John"), None, 1);
        data.observe(&candidate("Jon", "John"), None, 2);
        // The apply stage wrote "John"; the user changed it back to "Jon".
        let outcome = data.observe(&candidate("John", "Jon"), None, 3);
        assert!(matches!(outcome, Observation::InverseReverted(_)));
        assert!(data.corrections.is_empty(), "neither pair is kept");
        assert!(data.is_blocked("Jon", "John"));
        // The blocked pair is never learned again.
        assert_eq!(
            data.observe(&candidate("jon", "john"), None, 4),
            Observation::Blocked
        );
        assert!(data.corrections.is_empty());
    }

    #[test]
    fn inverse_of_a_manual_pair_keeps_it_and_learns_nothing() {
        let mut data = LearnedCorrections::default();
        data.add_manual("Jon", "John", 1);
        assert_eq!(
            data.observe(&candidate("John", "Jon"), None, 2),
            Observation::InverseOfManual
        );
        assert_eq!(data.corrections.len(), 1);
        assert!(find(&data, "Jon").is_applied());
    }

    #[test]
    fn manual_entries_are_never_overwritten_by_auto_relearns() {
        let mut data = LearnedCorrections::default();
        let manual = data.add_manual("Jon", "Jonathan", 1);
        assert_eq!(
            data.observe(&candidate("Jon", "John"), None, 2),
            Observation::ManualKept
        );
        assert_eq!(
            data.observe(&candidate("Jon", "Jonathan"), None, 3),
            Observation::ManualKept
        );
        assert_eq!(data.corrections, vec![manual]);
    }

    #[test]
    fn undo_group_removes_and_blocks_every_pair() {
        let mut data = LearnedCorrections::default();
        let ids: Vec<String> = [("Jon", "John"), ("Cubernetes", "Kubernetes")]
            .iter()
            .map(|(m, i)| match data.observe(&candidate(m, i), None, 1) {
                Observation::Suggested(id) => id,
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(data.reject(&ids, 5), 2);
        assert!(data.corrections.is_empty());
        assert!(data.is_blocked("Jon", "John"));
        assert!(data.is_blocked("Cubernetes", "Kubernetes"));
    }

    #[test]
    fn manual_add_lifts_a_block_and_unblock_works() {
        let mut data = LearnedCorrections::default();
        let Observation::Suggested(id) = data.observe(&candidate("Jon", "John"), None, 1) else {
            panic!()
        };
        data.reject(std::slice::from_ref(&id), 2);
        assert!(data.is_blocked("Jon", "John"));
        data.add_manual("Jon", "John", 3);
        assert!(!data.is_blocked("Jon", "John"));

        data.reject(std::slice::from_ref(&id), 4); // manual: removed, not blocked
        assert!(!data.is_blocked("Jon", "John"));
        data.observe(&candidate("Jon", "John"), None, 5);
        let Observation::Suggested(id) = data.observe(&candidate("Kates", "k8s"), None, 6) else {
            panic!()
        };
        data.reject(std::slice::from_ref(&id), 7);
        assert!(data.unblock(&id));
        assert!(!data.is_blocked("Kates", "k8s"));
    }

    #[test]
    fn new_target_for_auto_misheard_restarts_as_suggestion() {
        let mut data = LearnedCorrections::default();
        data.observe(&candidate("Jon", "John"), None, 1);
        data.observe(&candidate("Jon", "John"), None, 2);
        assert!(matches!(
            data.observe(&candidate("Jon", "Jonas"), None, 3),
            Observation::Suggested(_)
        ));
        let entry = find(&data, "Jon");
        assert_eq!(entry.intended, "Jonas");
        assert_eq!(entry.count, 1);
        assert_eq!(entry.status, CorrectionStatus::Suggested);
    }

    #[test]
    fn a_disabled_auto_pair_is_not_replaced_by_a_new_target() {
        let mut data = LearnedCorrections::default();
        data.observe(&candidate("Jon", "John"), None, 1);
        data.observe(&candidate("Jon", "John"), None, 2);
        let id = find(&data, "Jon").id.clone();
        assert!(data.set_enabled(&id, false));
        assert_eq!(
            data.observe(&candidate("Jon", "Jonas"), None, 3),
            Observation::DisabledKept
        );
        let entry = find(&data, "Jon");
        assert_eq!(entry.intended, "John");
        assert!(!entry.enabled);
    }

    #[test]
    fn applied_pairs_lists_only_active_enabled_pairs() {
        let mut data = LearnedCorrections::default();
        data.add_manual("Mark", "Marc", 1);
        data.observe(&candidate("Jon", "John"), None, 2); // suggestion
        let off = data.add_manual("kates", "k8s", 3);
        data.set_enabled(&off.id, false);
        assert_eq!(
            data.applied_pairs(),
            vec![("mark".to_string(), "marc".to_string())]
        );
    }

    #[test]
    fn legacy_list_merges_once() {
        let legacy = vec![LearnedCorrection::new(
            "a1",
            "b1",
            CorrectionSource::Manual,
            0,
        )];
        let mut data = LearnedCorrections::default();
        assert_eq!(data.merge_legacy(legacy.clone(), false), 1);
        // Already present: not duplicated.
        assert_eq!(data.merge_legacy(legacy.clone(), false), 0);
        // The user deletes it; a stale legacy copy must not resurrect it.
        data.corrections.clear();
        assert_eq!(data.merge_legacy(legacy, true), 0);
        assert!(data.corrections.is_empty());
    }

    #[test]
    fn snapshot_recovers_from_a_poisoned_lock() {
        let slot: Mutex<Option<Arc<LearnedCorrections>>> =
            Mutex::new(Some(Arc::new(LearnedCorrections {
                corrections: vec![LearnedCorrection::new(
                    "a",
                    "b",
                    CorrectionSource::Manual,
                    0,
                )],
                blocked: Vec::new(),
            })));
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = slot.lock().unwrap();
            panic!("poison the lock");
        }));
        assert!(slot.is_poisoned());
        assert_eq!(current(&slot).corrections.len(), 1);
    }

    #[test]
    fn legacy_entries_are_reclassified() {
        let manual = LearnedCorrection::new("a1", "b1", CorrectionSource::Manual, 0);
        let mut once = LearnedCorrection::new("a2", "b2", CorrectionSource::Auto, 0);
        once.lang = Some("en-DE".into());
        let mut twice = LearnedCorrection::new("a3", "b3", CorrectionSource::Auto, 0);
        twice.count = 2;
        let data = LearnedCorrections::from_legacy(vec![manual, once, twice]);
        let status: Vec<CorrectionStatus> = data.corrections.iter().map(|c| c.status).collect();
        assert_eq!(
            status,
            vec![
                CorrectionStatus::Active,
                CorrectionStatus::Suggested,
                CorrectionStatus::Active
            ]
        );
        assert_eq!(data.corrections[1].lang.as_deref(), Some("en"));
    }

    #[test]
    fn legacy_json_without_status_deserializes() {
        let json = serde_json::json!({
            "id": "x", "misheard": "a", "intended": "b", "count": 1,
            "last_seen": 0, "source": "auto", "enabled": true
        });
        let entry: LearnedCorrection = serde_json::from_value(json).unwrap();
        assert_eq!(entry.status, CorrectionStatus::Active);
        assert!(!entry.sentence_start);
    }

    #[test]
    fn eviction_prefers_suggestions_and_never_touches_manual() {
        let mut data = LearnedCorrections::default();
        for i in 0..MAX_LEARNED_CORRECTIONS - 1 {
            data.add_manual(&format!("m{i}"), &format!("n{i}"), 0);
        }
        data.observe(&candidate("old", "suggestion"), None, 1);
        assert_eq!(data.corrections.len(), MAX_LEARNED_CORRECTIONS);
        // The new suggestion evicts the old one, not a manual entry.
        data.observe(&candidate("fresh", "suggestion"), None, 2);
        assert_eq!(data.corrections.len(), MAX_LEARNED_CORRECTIONS);
        assert!(!data.corrections.iter().any(|c| c.misheard == "old"));
        assert!(data.corrections.iter().any(|c| c.misheard == "fresh"));
        // Fill the last slot with manual: no auto pair left to evict.
        data.remove(&correction_id("fresh", "suggestion"));
        data.add_manual("last", "manual", 3);
        assert_eq!(
            data.observe(&candidate("another", "one"), None, 4),
            Observation::Full
        );
    }
}
