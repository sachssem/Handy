//! Post-paste learning session — the live capture stage (fork feature:
//! voice-control).
//!
//! After a transcription is pasted, [`begin_session`] opens a short learning
//! window on the app that was pasted into: it snapshots the pasted text and the
//! target pid, then watches the focused field for a manual correction, diffs the
//! edit against the snapshot ([`super::differ`]) and, when a gated candidate is
//! stable, learns the `misheard → intended` pair.
//!
//! ## Trigger strategy: poll, don't hook
//!
//! The window could be driven either by subscribing to "user typed after paste"
//! key events or by re-reading the field on a timer. We poll, deliberately:
//!
//! - The existing shortcut backend (`handy_keys`) exposes **no** general
//!   key-event callback we could piggyback on — its `HotkeyManager` only
//!   dispatches the specific hotkeys it has registered. The one general-key
//!   source, `KeyboardListener`, is a *second* global listener created on
//!   demand; standing one up for the whole window is exactly the second global
//!   input tap the design doc warns can conflict with the shortcut backend
//!   (risk #5).
//! - Reading a single AX value every few seconds is cheap and conflict-free.
//!
//! So a background thread re-reads the snapshotted app's focused field every
//! [`POLL_INTERVAL`] for up to [`WINDOW`], running the differ each tick. It
//! finishes early when a gated candidate is seen twice in a row (stable), or
//! when the field goes secure / the app quits. A newer paste supersedes any
//! in-flight session (single active session, tracked by [`GENERATION`]).
//!
//! Only macOS has the Accessibility read; elsewhere [`begin_session`] is a
//! no-op.

use serde::{Deserialize, Serialize};
use specta::Type;

/// Emitted when a correction is learned automatically, for the Phase C toast.
/// No frontend listener exists yet — this ships the event contract only.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LearnedCorrectionEvent {
    pub id: String,
    pub misheard: String,
    pub intended: String,
}

/// Fired when the learned-corrections list changes behind the review UI's back —
/// currently a toast Undo removing a pair. The settings window listens for it
/// and re-fetches settings so its table never shows a stale entry. (Auto-learn
/// additions ride on [`LearnedCorrectionEvent`], which the settings window also
/// listens to.)
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LearnedCorrectionsChanged {}

/// Whether `current` field text still looks like an edited copy of the pasted
/// `original`, as opposed to the user having navigated away or cleared the
/// field. A cheap guard run before diffing so an unrelated field never feeds the
/// differ.
///
/// Related when either span still contains the other (text was added around the
/// paste) or most of the original's words survive (a word-level correction keeps
/// all but the fixed word). Pure and unit-tested.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn is_related_to_snapshot(original: &str, current: &str) -> bool {
    let original_words = normalized_words(original);
    if original_words.is_empty() {
        return false;
    }
    let current_words = normalized_words(current);
    if current_words.is_empty() {
        // Field cleared out entirely — not a correction, not related.
        return false;
    }

    // Whole-word containment: one word list appears as a contiguous run of the
    // other (text added around the paste). Token-based, not substring, so a
    // short original like `yes` never counts as contained in `yesterday`.
    if contains_word_run(&current_words, &original_words)
        || contains_word_run(&original_words, &current_words)
    {
        return true;
    }

    // Fraction of the original's words still present in the current field.
    let surviving = original_words
        .iter()
        .filter(|word| current_words.contains(word))
        .count();
    (surviving as f64) / (original_words.len() as f64) >= RELATED_WORD_RATIO
}

/// Whether `needle` occurs as a contiguous run of whole words inside `haystack`.
/// An empty `needle` is not considered contained.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn contains_word_run(haystack: &[String], needle: &[String]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Minimum share of the original's words that must survive for the field to
/// count as a still-related edit rather than a navigation away.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const RELATED_WORD_RATIO: f64 = 0.5;

/// Lowercased whitespace-split word list, diacritics preserved (so `München`
/// and `Munchen` stay distinct, matching the differ's normalization).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn normalized_words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|word| {
            word.chars()
                .filter(|c| c.is_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect::<String>()
        })
        .filter(|word| !word.is_empty())
        .collect()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{is_related_to_snapshot, LearnedCorrectionEvent};
    use crate::correction_learning::ax_reader::{self, FocusRead};
    use crate::correction_learning::differ::{self, Candidate, GateProfile, PhoneticLang};
    use crate::correction_learning::resolved_language;
    use crate::correction_learning::store::{self, CorrectionSource, LearnedCorrection};
    use crate::settings::{self, AppSettings, PasteMethod};
    use log::info;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use tauri::AppHandle;
    use tauri_specta::Event;

    /// How often the snapshotted field is re-read within the window. Const (the
    /// window length itself is the user-facing `learn_corrections_window_secs`).
    const POLL_INTERVAL: Duration = Duration::from_secs(4);
    /// Bounds for the configurable learning window, clamped so a stray setting
    /// value can neither close the window instantly nor keep the poll thread
    /// alive indefinitely.
    const MIN_WINDOW_SECS: u32 = 10;
    const MAX_WINDOW_SECS: u32 = 300;

    /// Bumped on every `begin_session`; a running session exits once its
    /// generation is no longer current, so a newer paste cleanly supersedes it.
    static GENERATION: AtomicU64 = AtomicU64::new(0);

    /// The learning window length from settings, clamped to a sane range.
    fn window(settings: &AppSettings) -> Duration {
        let secs = settings
            .learn_corrections_window_secs
            .clamp(MIN_WINDOW_SECS, MAX_WINDOW_SECS);
        Duration::from_secs(secs as u64)
    }

    /// Which phonetic algorithm the borderline gate should use for a resolved
    /// language code (see [`resolved_language`]). German → Kölner Phonetik,
    /// everything else → Double Metaphone.
    fn phonetic_lang(lang_code: &str) -> PhoneticLang {
        if lang_code.starts_with("de") {
            PhoneticLang::German
        } else {
            PhoneticLang::Other
        }
    }

    /// Snapshot a just-pasted transcription and open the learning window on the
    /// app it was pasted into. Runs on the main thread (the paste callsite), so
    /// the frontmost pid is current; the poll loop then runs off-thread.
    pub fn begin_session(app: &AppHandle, original: String) {
        let settings = settings::get_settings(app);
        if !settings.learn_corrections_enabled {
            return;
        }
        // Only paste methods that actually insert into the focused field can be
        // corrected in place: `None` pastes nothing, and `ExternalScript` may
        // route the text anywhere, so neither leaves an editable field to diff.
        if matches!(
            settings.paste_method,
            PasteMethod::None | PasteMethod::ExternalScript
        ) {
            return;
        }
        // Resolve the paste target now, on the main thread. Thereafter the AX
        // element is re-created from this pid, so the session follows the
        // snapshotted app rather than whatever becomes frontmost later.
        let pid = match ax_reader::frontmost_pid() {
            Some(pid) => pid,
            None => return,
        };
        // Snapshot the target app's identity too, so a recycled pid (the app
        // quit and the OS reassigned the number) is caught on the next read.
        let app_name = ax_reader::process_name(pid);

        // Snapshot the gate configuration at paste time, alongside the text.
        let lang_code = resolved_language(&settings);
        let params = SessionParams {
            window: window(&settings),
            profile: GateProfile::for_aggressiveness(settings.learn_corrections_aggressiveness),
            lang: phonetic_lang(&lang_code),
            lang_code,
        };

        let my_generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let app = app.clone();
        std::thread::spawn(move || {
            run_session(app, my_generation, pid, app_name, original, params)
        });
    }

    /// The gate configuration snapshotted for one learning window.
    struct SessionParams {
        window: Duration,
        profile: GateProfile,
        lang: PhoneticLang,
        /// Resolved language code stored on any pair learned this window, so the
        /// apply stage can gate it to the same language.
        lang_code: String,
    }

    /// The polling window: re-read the target field, diff, and commit a stable
    /// gated candidate. Returns on commit, teardown, expiry, or supersession.
    fn run_session(
        app: AppHandle,
        generation: u64,
        pid: i32,
        app_name: Option<String>,
        original: String,
        params: SessionParams,
    ) {
        let started = Instant::now();
        let mut last_candidate: Option<Candidate> = None;
        let mut last_text: Option<String> = None;

        loop {
            std::thread::sleep(POLL_INTERVAL);

            // A newer paste opened a fresh session — drop this one.
            if GENERATION.load(Ordering::SeqCst) != generation {
                return;
            }
            if started.elapsed() >= params.window {
                return;
            }

            match ax_reader::read_focused(pid, app_name.as_deref()) {
                // Secure field or the app quit → tear the session down silently.
                FocusRead::Secure | FocusRead::AppGone => return,
                // Nothing readable this tick; reset stability and force the next
                // text read to be diffed afresh.
                FocusRead::NoSignal => {
                    last_candidate = None;
                    last_text = None;
                }
                FocusRead::Text(current) => {
                    // Byte-identical to the previous tick: the field has settled,
                    // so skip the relatedness + diff work. A candidate already
                    // pending from the previous tick is now confirmed stable.
                    if last_text.as_deref() == Some(current.as_str()) {
                        if let Some(candidate) = last_candidate.take() {
                            commit(&app, candidate, &params.lang_code);
                            return;
                        }
                        continue;
                    }
                    last_text = Some(current.clone());

                    if !is_related_to_snapshot(&original, &current) {
                        // Field no longer relates to the paste (navigated away).
                        last_candidate = None;
                        continue;
                    }
                    match differ::extract_correction(
                        &original,
                        &current,
                        &params.profile,
                        params.lang,
                    ) {
                        Some(candidate) => {
                            // Require the same candidate on two consecutive reads
                            // so we learn only after the edit has settled.
                            if last_candidate.as_ref() == Some(&candidate) {
                                commit(&app, candidate, &params.lang_code);
                                return;
                            }
                            last_candidate = Some(candidate);
                        }
                        None => last_candidate = None,
                    }
                }
            }
        }
    }

    /// Learn a gated candidate: log-only under the dry-run switch, otherwise
    /// upsert it into settings and emit the toast event. `lang_code` is the
    /// resolved language the pair was learned for, recorded so the apply stage
    /// only uses it for the same language.
    fn commit(app: &AppHandle, candidate: Candidate, lang_code: &str) {
        let mut settings = settings::get_settings(app);

        // Dry-run soak: run the whole pipeline but only log the would-be pair.
        if settings.learn_corrections_log_only {
            info!(
                "would-learn: {} -> {}",
                candidate.misheard, candidate.intended
            );
            return;
        }

        let mut entry = LearnedCorrection::new(
            &candidate.misheard,
            &candidate.intended,
            CorrectionSource::Auto,
            chrono::Utc::now().timestamp(),
        );
        entry.lang = Some(lang_code.to_string());
        let id = store::upsert(&mut settings.learned_corrections, entry);
        settings::write_settings(app, settings);

        info!(
            "learned correction: {} -> {}",
            candidate.misheard, candidate.intended
        );
        let event = LearnedCorrectionEvent {
            id,
            misheard: candidate.misheard,
            intended: candidate.intended,
        };
        if let Err(err) = event.emit(app) {
            log::error!("Failed to emit learned-correction event: {}", err);
        }
        // The toast webview has just received the event; reveal its window.
        crate::correction_learning::toast::show_learned_toast(app);
    }
}

#[cfg(target_os = "macos")]
pub use imp::begin_session;

/// No-op on platforms without an Accessibility read — the feature is silently
/// absent there (the deterministic apply stage still works everywhere).
#[cfg(not(target_os = "macos"))]
pub fn begin_session(_app: &tauri::AppHandle, _original: String) {}

#[cfg(test)]
mod tests {
    use super::is_related_to_snapshot;

    #[test]
    fn single_word_correction_stays_related() {
        assert!(is_related_to_snapshot(
            "Ich war in Munchen",
            "Ich war in München"
        ));
    }

    #[test]
    fn appended_text_stays_related() {
        // User kept the paste and typed more after it.
        assert!(is_related_to_snapshot(
            "send it to Jon",
            "send it to Jon please"
        ));
    }

    #[test]
    fn navigating_to_unrelated_field_is_not_related() {
        assert!(!is_related_to_snapshot(
            "Ich war in Munchen",
            "completely different sentence here"
        ));
    }

    #[test]
    fn cleared_field_is_not_related() {
        assert!(!is_related_to_snapshot("send it to Jon", ""));
    }

    #[test]
    fn empty_original_is_never_related() {
        assert!(!is_related_to_snapshot("", "anything at all"));
    }

    #[test]
    fn substring_word_is_not_related() {
        // `yes` must not count as contained in `yesterday`: token containment,
        // not raw substring.
        assert!(!is_related_to_snapshot("yes", "yesterday afternoon"));
    }
}
