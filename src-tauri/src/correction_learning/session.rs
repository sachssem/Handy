//! Post-paste learning session — the live capture stage (fork feature:
//! voice-control).
//!
//! After a transcription is pasted, [`begin_session`] opens a short learning
//! window on the app that was pasted into: it snapshots the pasted text and the
//! target pid, then watches the focused field for a manual correction, diffs the
//! edit against the snapshot ([`super::differ`]) and, when a gated candidate is
//! stable, learns the `misheard → intended` pair.
//!
//! ## Trigger strategy: field events, poll as fallback
//!
//! We never stand up a second global key listener — the existing shortcut
//! backend (`handy_keys`) exposes no general key-event callback to piggyback on,
//! and a second global input tap is exactly the conflict the design doc warns
//! against (risk #5). Instead the session watches the *pinned field itself*:
//!
//! - An `AXObserver` on the snapshotted element (see
//!   [`ax_reader::create_value_change_observer`]) wakes the session on every
//!   value change (and on the element's destruction), so a correction typed and
//!   submitted **inside one poll interval** is still read before the field
//!   blurs/clears — the failure a pure timer could never close.
//! - A [`POLL_INTERVAL`] timer remains as a fallback: some apps don't emit AX
//!   notifications reliably, and if the observer can't be created at all (no AX
//!   permission, odd app) the session degrades to pure polling.
//!
//! So a background thread waits for either signal, then re-reads the field and
//! runs the differ, for up to [`WINDOW`]. A burst of per-keystroke wakeups is
//! coalesced to at most one read per [`DEBOUNCE`]. It finishes early when a gated
//! candidate settles, or when the field goes secure / the app quits. A newer
//! paste supersedes any in-flight session (single active session, tracked by
//! [`GENERATION`]).
//!
//! Only macOS has the Accessibility read; elsewhere [`begin_session`] is a
//! no-op.

use crate::correction_learning::ax_reader::FocusRead;
use crate::correction_learning::differ::{self, Candidate, GateProfile, PhoneticLang};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::Duration;

/// Emitted when a correction is learned automatically, for the Phase C toast.
/// No frontend listener exists yet — this ships the event contract only.
#[derive(Clone, Debug, Serialize, Deserialize, Type, tauri_specta::Event)]
pub struct LearnedCorrectionEvent {
    pub id: String,
    pub misheard: String,
    pub intended: String,
    /// Dry-run soak (`learn_corrections_log_only`): the pair was *not* persisted,
    /// so the toast marks it as a trial and hides Undo. `false` for real
    /// learned pairs that were stored.
    pub trial: bool,
    /// How many *additional* corrections were committed alongside this one in the
    /// same edit. One toast shows this pair verbatim and appends "+{extra} more"
    /// when non-zero (several independent word fixes that settled together). `0`
    /// for the common single-correction case.
    pub extra: u32,
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
    let original_words = differ::normalized_words(original);
    if original_words.is_empty() {
        return false;
    }
    let current_words = differ::normalized_words(current);
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

/// The decision one poll tick reaches from a field read, kept pure so the poll
/// loop is a thin driver and the branching is unit-testable. `Continue` also
/// carries the tracked state the next tick should start from.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug)]
enum TickDecision {
    /// Stop the session silently (secure field, the target app quit, or focus
    /// moved to a different element).
    Teardown,
    /// The candidate set has settled — learn every pair, then stop.
    Commit(Vec<Candidate>),
    /// Keep polling with this tracked state.
    Continue {
        last_text: Option<String>,
        last_candidates: Vec<Candidate>,
        /// Set only when this tick saw a *changed, related* edit that produced no
        /// gated candidate — carries the run/gate breakdown for one diagnostic
        /// log line. `None` on every other path (unchanged text, unrelated field,
        /// no signal), so the driver logs the "differ rejected" case and stays
        /// quiet otherwise.
        no_candidate: Option<NoCandidate>,
    },
}

/// Why a changed, related edit produced no learnable candidate: the run/gate
/// breakdown the driver logs so the session's log can tell "differ rejected the
/// edit" apart from "never saw the edit".
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NoCandidate {
    /// Total change regions the edit had (a large count means a reformulation).
    runs: usize,
    /// Substitution runs that failed to yield a learned pair.
    gated_out: usize,
}

/// Decide what one poll tick does, given the field `read`, the pasted `original`
/// and the state carried from the previous tick. Pure: side effects (learning,
/// sleeping, generation/expiry checks) stay in the driver.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn decide_tick(
    read: FocusRead,
    original: &str,
    last_text: Option<String>,
    last_candidates: Vec<Candidate>,
    profile: &GateProfile,
    lang: PhoneticLang,
) -> TickDecision {
    match read {
        // Secure field or the app quit → tear the session down silently.
        FocusRead::Secure | FocusRead::AppGone => TickDecision::Teardown,
        // Focus moved to a different element (blur/submit). The field's last
        // observed state is final — nothing can edit it in place anymore — so a
        // candidate set that already passed the gates on the previous read no
        // longer needs its confirming second read: commit it now. This is what
        // makes "correct, then immediately submit" learnable.
        FocusRead::FocusChanged => {
            if last_candidates.is_empty() {
                TickDecision::Teardown
            } else {
                TickDecision::Commit(last_candidates)
            }
        }
        // Nothing readable this tick; reset stability and force the next text
        // read to be diffed afresh.
        FocusRead::NoSignal => TickDecision::Continue {
            last_text: None,
            last_candidates: Vec::new(),
            no_candidate: None,
        },
        FocusRead::Text(current) => {
            // Byte-identical to the previous tick: the field has settled, so skip
            // the relatedness + diff work. A candidate set already pending from
            // the previous tick is now confirmed stable.
            if last_text.as_deref() == Some(current.as_str()) {
                return if last_candidates.is_empty() {
                    TickDecision::Continue {
                        last_text,
                        last_candidates,
                        no_candidate: None,
                    }
                } else {
                    TickDecision::Commit(last_candidates)
                };
            }

            if !is_related_to_snapshot(original, &current) {
                // Field no longer relates to the paste (navigated away). Not a
                // rejected edit, so no diagnostic.
                return TickDecision::Continue {
                    last_text: Some(current),
                    last_candidates: Vec::new(),
                    no_candidate: None,
                };
            }

            let candidates = differ::extract_corrections(original, &current, profile, lang);
            if candidates.is_empty() {
                // A changed, related edit the differ could not turn into a
                // candidate — surface the run/gate breakdown so the log tells a
                // rejection apart from "never saw the edit".
                let (runs, substitutions) = differ::change_run_counts(original, &current);
                return TickDecision::Continue {
                    last_text: Some(current),
                    last_candidates: Vec::new(),
                    no_candidate: Some(NoCandidate {
                        runs,
                        gated_out: substitutions,
                    }),
                };
            }
            // Require the same set on two consecutive reads so we learn only
            // after the edit has settled; a changed set restarts confirmation.
            if last_candidates == candidates {
                TickDecision::Commit(candidates)
            } else {
                TickDecision::Continue {
                    last_text: Some(current),
                    last_candidates: candidates,
                    no_candidate: None,
                }
            }
        }
    }
}

/// How long to wait before the next field read so a burst of value-change
/// wakeups collapses into at most one read per `debounce`. `None` (no prior
/// read) or a gap already `>= debounce` means read immediately. Pure and
/// unit-tested; the event-driven loop can otherwise wake once per keystroke.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn coalesce_delay(since_last_read: Option<Duration>, debounce: Duration) -> Duration {
    match since_last_read {
        Some(elapsed) if elapsed < debounce => debounce - elapsed,
        _ => Duration::ZERO,
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{coalesce_delay, decide_tick, LearnedCorrectionEvent, TickDecision};
    use crate::correction_learning::ax_reader;
    use crate::correction_learning::differ::{Candidate, GateProfile, PhoneticLang};
    use crate::correction_learning::resolved_language;
    use crate::correction_learning::store::{self, CorrectionSource, LearnedCorrection};
    use crate::settings::{self, AppSettings, PasteMethod};
    use log::{debug, info};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use tauri::AppHandle;
    use tauri_specta::Event;

    /// How often the snapshotted field is re-read within the window. Const (the
    /// window length itself is the user-facing `learn_corrections_window_secs`).
    /// 2s keeps the type-fix-then-submit flow inside one confirming read; the
    /// AX read is cheap (single element attribute fetch).
    const POLL_INTERVAL: Duration = Duration::from_secs(2);
    /// Minimum spacing between two field reads. The AX observer can wake the loop
    /// on every keystroke; this coalesces such a burst into at most one read per
    /// 100 ms (a read is one AX fetch + a cheap diff) without adding latency to
    /// the common single-edit case.
    const DEBOUNCE: Duration = Duration::from_millis(100);
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
            debug!("learn: no session — feature disabled");
            return;
        }
        // Only paste methods that actually insert into the focused field can be
        // corrected in place: `None` pastes nothing, and `ExternalScript` may
        // route the text anywhere, so neither leaves an editable field to diff.
        if matches!(
            settings.paste_method,
            PasteMethod::None | PasteMethod::ExternalScript
        ) {
            debug!(
                "learn: no session — paste method {:?} leaves no editable field",
                settings.paste_method
            );
            return;
        }
        // Resolve the paste target now, on the main thread. Thereafter the AX
        // element is re-created from this pid, so the session follows the
        // snapshotted app rather than whatever becomes frontmost later.
        let pid = match ax_reader::frontmost_pid() {
            Some(pid) => pid,
            None => {
                debug!("learn: no session — no frontmost pid resolved");
                return;
            }
        };
        // Snapshot the target app's identity too, so a recycled pid (the app
        // quit and the OS reassigned the number) is caught on the next read.
        let app_name = ax_reader::process_name(pid);
        // Pin the exact focused element the text was pasted into, so later reads
        // can confirm the user is still editing that same field (not a different
        // one in the same app). Without it we cannot attribute an edit, so skip
        // the session silently.
        let focus = match ax_reader::snapshot_focused_element(pid) {
            Some(focus) => focus,
            None => {
                // Most often an AX-permission gap: no focused element could be
                // read for the pasted-into app. This is the quiet failure that
                // makes the whole feature look dead, so it is logged.
                debug!(
                    "learn: no session — focused-element snapshot failed for pid {} (AX permission?)",
                    pid
                );
                return;
            }
        };

        // Snapshot the gate configuration at paste time, alongside the text.
        let lang_code = resolved_language(&settings);
        let params = SessionParams {
            window: window(&settings),
            profile: GateProfile::for_aggressiveness(settings.learn_corrections_aggressiveness),
            lang: phonetic_lang(&lang_code),
            lang_code,
        };

        let my_generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        // Content-redacted: only the pasted length and the window/poll timing,
        // never the pasted text itself.
        debug!(
            "learn: session {} started — pid {}, {} pasted chars, window {}s, poll {}s",
            my_generation,
            pid,
            original.chars().count(),
            params.window.as_secs(),
            POLL_INTERVAL.as_secs()
        );
        let app = app.clone();
        std::thread::spawn(move || {
            run_session(app, my_generation, pid, app_name, focus, original, params)
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

    /// The polling window: re-read the target field, run [`decide_tick`], and
    /// apply its decision. Returns on commit, teardown, expiry, or supersession.
    fn run_session(
        app: AppHandle,
        generation: u64,
        pid: i32,
        app_name: Option<String>,
        focus: ax_reader::FocusedSnapshot,
        original: String,
        params: SessionParams,
    ) {
        let started = Instant::now();
        let mut last_candidates: Vec<Candidate> = Vec::new();
        let mut last_text: Option<String> = None;
        let mut last_read: Option<Instant> = None;
        // The last "edit seen but nothing learned" breakdown we logged, so a
        // keystroke burst whose extraction outcome never changes is logged once
        // rather than per wakeup.
        let mut last_no_candidate: Option<(usize, usize)> = None;

        // Wake on every value change of the pinned field so a correction typed
        // and submitted inside one poll interval is still read before it blurs.
        // Created on this (session) thread because its run-loop source attaches
        // to this thread's run loop. `None` → the app emits no usable AX signal
        // or has no permission, so we fall back to pure `POLL_INTERVAL` polling.
        let observer = ax_reader::create_value_change_observer(pid, &focus);
        if observer.is_some() {
            debug!(
                "learn: session {} watching field via AX observer (poll fallback {}s)",
                generation,
                POLL_INTERVAL.as_secs()
            );
        } else {
            debug!(
                "learn: session {} AX observer unavailable — polling every {}s",
                generation,
                POLL_INTERVAL.as_secs()
            );
        }

        loop {
            // Wait for a field value change or the poll interval, whichever comes
            // first. Without an observer this is a plain interval sleep.
            match &observer {
                Some(observer) => observer.wait(POLL_INTERVAL),
                None => std::thread::sleep(POLL_INTERVAL),
            }

            // A newer paste opened a fresh session — drop this one.
            if GENERATION.load(Ordering::SeqCst) != generation {
                debug!(
                    "learn: session {} teardown — superseded by a newer paste",
                    generation
                );
                return;
            }
            if started.elapsed() >= params.window {
                debug!("learn: session {} teardown — window elapsed", generation);
                return;
            }

            // Coalesce a burst of per-keystroke wakeups: keep at least DEBOUNCE
            // between actual reads. On a plain poll tick the gap already exceeds
            // it, so this only bites during event storms.
            let delay = coalesce_delay(last_read.map(|at| at.elapsed()), DEBOUNCE);
            if !delay.is_zero() {
                std::thread::sleep(delay);
            }
            last_read = Some(Instant::now());

            let had_candidate = !last_candidates.is_empty();
            let read = ax_reader::read_focused(pid, app_name.as_deref(), &focus);
            match decide_tick(
                read,
                &original,
                last_text.take(),
                std::mem::take(&mut last_candidates),
                &params.profile,
                params.lang,
            ) {
                TickDecision::Teardown => {
                    debug!(
                        "learn: session {} teardown — focus lost, secure field, or app gone",
                        generation
                    );
                    return;
                }
                TickDecision::Commit(candidates) => {
                    debug!(
                        "learn: session {} candidate set stable ({} pair(s)) — committing",
                        generation,
                        candidates.len()
                    );
                    commit(&app, candidates, &params.lang_code);
                    return;
                }
                TickDecision::Continue {
                    last_text: next_text,
                    last_candidates: next_candidates,
                    no_candidate,
                } => {
                    // Content-redacted diagnostic: a changed, related edit the
                    // differ rejected. Logged only when the outcome changed vs the
                    // last logged one, so a keystroke burst does not spam the log.
                    if let Some(diag) = no_candidate {
                        let key = (diag.runs, diag.gated_out);
                        if last_no_candidate != Some(key) {
                            debug!(
                                "learn: session {} edit seen but no gated candidate (runs={}, gated_out={})",
                                generation, diag.runs, diag.gated_out
                            );
                            last_no_candidate = Some(key);
                        }
                    } else if !next_candidates.is_empty() {
                        // Back in a learnable state; let a later dry spell log again.
                        last_no_candidate = None;
                    }

                    // Content-redacted stability transitions: a fresh candidate
                    // set now awaits a confirming read, or a pending one fell away.
                    match (had_candidate, next_candidates.is_empty()) {
                        (false, false) => debug!(
                            "learn: session {} candidate set found ({} pair(s), pending confirmation)",
                            generation,
                            next_candidates.len()
                        ),
                        (true, true) => debug!(
                            "learn: session {} pending candidate set dropped before confirmation",
                            generation
                        ),
                        _ => {}
                    }
                    last_text = next_text;
                    last_candidates = next_candidates;
                }
            }
        }
    }

    /// Learn a settled candidate set: log-only under the dry-run switch,
    /// otherwise upsert every pair into settings and emit the toast event.
    /// `lang_code` is the resolved language the pairs were learned for, recorded
    /// so the apply stage only uses them for the same language.
    ///
    /// Every pair is persisted (or trial-logged), but the toast window shows one
    /// pair at a time, so it surfaces the *first* pair verbatim and carries the
    /// count of the rest as `extra` (rendered as "+N more") — a later toast per
    /// pair would just overwrite this one.
    fn commit(app: &AppHandle, candidates: Vec<Candidate>, lang_code: &str) {
        if candidates.is_empty() {
            return;
        }
        // The toast shows the first pair; the others are summarised as "+N".
        let extra = (candidates.len() - 1) as u32;
        let mut settings = settings::get_settings(app);

        // Dry-run soak: run the whole pipeline but only log the would-be pairs.
        // The pair is user text, so at info level it is redacted to lengths; the
        // verbatim pair is logged only at debug! level, which reaches the log
        // file / live viewer solely when the user has raised the log level.
        if settings.learn_corrections_log_only {
            let mut first_event: Option<LearnedCorrectionEvent> = None;
            for candidate in candidates {
                info!(
                    "would-learn: misheard {} chars -> intended {} chars",
                    candidate.misheard.chars().count(),
                    candidate.intended.chars().count()
                );
                debug!(
                    "would-learn (verbatim): {} -> {}",
                    candidate.misheard, candidate.intended
                );
                if first_event.is_none() {
                    // A soak you can't observe reads as a broken feature, so still
                    // show the toast — marked as a trial. We persist nothing and,
                    // crucially, emit no `LearnedCorrectionEvent`: the settings
                    // window refreshes its stored list on that event, and nothing
                    // was stored. The id is the pair's would-be content hash,
                    // purely so the payload is well-formed; the trial toast hides
                    // Undo, so it is never used to remove anything.
                    first_event = Some(LearnedCorrectionEvent {
                        id: LearnedCorrection::new(
                            &candidate.misheard,
                            &candidate.intended,
                            CorrectionSource::Auto,
                            chrono::Utc::now().timestamp(),
                        )
                        .id,
                        misheard: candidate.misheard,
                        intended: candidate.intended,
                        trial: true,
                        extra,
                    });
                }
            }
            if let Some(event) = first_event {
                crate::correction_learning::toast::set_pending_learned_toast(event.clone());
                // Also emit: an already-created toast webview refreshes its
                // content through the event, not the stash (which only the cold
                // first mount consumes). The settings window reacting with a
                // redundant refetch of an unchanged list is harmless.
                if let Err(err) = event.emit(app) {
                    log::error!("Failed to emit learned-correction trial event: {}", err);
                }
                crate::correction_learning::toast::show_learned_toast(app);
            }
            return;
        }

        let mut first_event: Option<LearnedCorrectionEvent> = None;
        for candidate in candidates {
            let mut entry = LearnedCorrection::new(
                &candidate.misheard,
                &candidate.intended,
                CorrectionSource::Auto,
                chrono::Utc::now().timestamp(),
            );
            entry.lang = Some(lang_code.to_string());
            let id = store::upsert(&mut settings.learned_corrections, entry);

            // Redacted at info level (the pair is user text); verbatim only at
            // debug! level, gated by the user's log level as above. The id is a
            // non-reversible content hash, so it is safe to log for correlation.
            info!(
                "learned correction {}: misheard {} chars -> intended {} chars",
                id,
                candidate.misheard.chars().count(),
                candidate.intended.chars().count()
            );
            debug!(
                "learned correction {} (verbatim): {} -> {}",
                id, candidate.misheard, candidate.intended
            );
            if first_event.is_none() {
                first_event = Some(LearnedCorrectionEvent {
                    id,
                    misheard: candidate.misheard,
                    intended: candidate.intended,
                    trial: false,
                    extra,
                });
            }
        }
        settings::write_settings(app, settings);

        if let Some(event) = first_event {
            // Stash the pair before showing, so the toast webview — created lazily
            // on this very correction — can pick it up on mount even if it wasn't
            // yet listening when the event below was emitted.
            crate::correction_learning::toast::set_pending_learned_toast(event.clone());
            if let Err(err) = event.emit(app) {
                log::error!("Failed to emit learned-correction event: {}", err);
            }
            // Reveal the toast window (created on first use here).
            crate::correction_learning::toast::show_learned_toast(app);
        }
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
    use super::{coalesce_delay, decide_tick, is_related_to_snapshot, TickDecision};
    use crate::correction_learning::ax_reader::FocusRead;
    use crate::correction_learning::differ::{
        Aggressiveness, Candidate, GateProfile, PhoneticLang,
    };
    use std::time::Duration;

    fn profile() -> GateProfile {
        GateProfile::for_aggressiveness(Aggressiveness::Balanced)
    }

    fn tick(read: FocusRead, last_text: Option<&str>, last: Vec<Candidate>) -> TickDecision {
        tick_for("send it to Jon", read, last_text, last)
    }

    /// [`tick`] with a caller-chosen `original`, for the multi-word cases.
    fn tick_for(
        original: &str,
        read: FocusRead,
        last_text: Option<&str>,
        last: Vec<Candidate>,
    ) -> TickDecision {
        decide_tick(
            read,
            original,
            last_text.map(str::to_string),
            last,
            &profile(),
            PhoneticLang::Other,
        )
    }

    fn candidate(misheard: &str, intended: &str) -> Candidate {
        Candidate {
            misheard: misheard.into(),
            intended: intended.into(),
        }
    }

    #[test]
    fn secure_gone_and_focus_changed_reads_tear_down() {
        assert!(matches!(
            tick(FocusRead::Secure, None, Vec::new()),
            TickDecision::Teardown
        ));
        assert!(matches!(
            tick(FocusRead::AppGone, None, Vec::new()),
            TickDecision::Teardown
        ));
        assert!(matches!(
            tick(FocusRead::FocusChanged, None, Vec::new()),
            TickDecision::Teardown
        ));
    }

    #[test]
    fn focus_change_commits_a_pending_candidate() {
        // Blur/submit after the fix was seen once: the field state is final,
        // so the pending set commits instead of being torn down.
        let pending = vec![candidate("Jon", "John")];
        match tick(
            FocusRead::FocusChanged,
            Some("send it to John"),
            pending.clone(),
        ) {
            TickDecision::Commit(committed) => assert_eq!(committed, pending),
            other => panic!("expected commit, got {other:?}"),
        }
    }

    #[test]
    fn secure_field_never_commits_a_pending_candidate() {
        assert!(matches!(
            tick(
                FocusRead::Secure,
                Some("send it to John"),
                vec![candidate("Jon", "John")]
            ),
            TickDecision::Teardown
        ));
    }

    #[test]
    fn no_signal_resets_tracked_state() {
        match tick(
            FocusRead::NoSignal,
            Some("send it to John"),
            vec![candidate("Jon", "John")],
        ) {
            TickDecision::Continue {
                last_text: None,
                last_candidates,
                no_candidate: None,
            } => assert!(last_candidates.is_empty()),
            other => panic!("expected a reset continue, got {other:?}"),
        }
    }

    #[test]
    fn first_edit_is_pending_then_committed_on_stable_read() {
        // First read of the edit: candidate pending, not yet committed.
        let first = tick(FocusRead::Text("send it to John".into()), None, Vec::new());
        let candidates = match first {
            TickDecision::Continue {
                last_candidates, ..
            } if !last_candidates.is_empty() => last_candidates,
            other => panic!("expected a pending candidate, got {other:?}"),
        };
        assert_eq!(candidates, vec![candidate("Jon", "John")]);
        // Same set seen again → commit.
        assert!(matches!(
            tick(
                FocusRead::Text("send it to John".into()),
                Some("send it to John"),
                candidates,
            ),
            TickDecision::Commit(_)
        ));
    }

    #[test]
    fn byte_identical_read_confirms_pending_candidate() {
        // Field text unchanged since the pending tick → settled → commit.
        assert!(matches!(
            tick(
                FocusRead::Text("send it to John".into()),
                Some("send it to John"),
                vec![candidate("Jon", "John")],
            ),
            TickDecision::Commit(_)
        ));
    }

    #[test]
    fn unrelated_field_drops_candidate() {
        match tick(
            FocusRead::Text("completely different text now".into()),
            None,
            Vec::new(),
        ) {
            TickDecision::Continue {
                last_candidates,
                no_candidate: None,
                ..
            } => assert!(last_candidates.is_empty()),
            other => panic!("expected a dropped-candidate continue, got {other:?}"),
        }
    }

    #[test]
    fn two_word_fix_yields_both_candidates() {
        // Two separately misheard words fixed in one sentence become a pending
        // set of both pairs, in document order.
        match tick_for(
            "send rahndom to Jon",
            FocusRead::Text("send random to John".into()),
            None,
            Vec::new(),
        ) {
            TickDecision::Continue {
                last_candidates, ..
            } => assert_eq!(
                last_candidates,
                vec![candidate("rahndom", "random"), candidate("Jon", "John")]
            ),
            other => panic!("expected a pending set, got {other:?}"),
        }
    }

    #[test]
    fn same_candidate_set_on_two_reads_commits() {
        // The field wiggled (a trailing space) but the gated set is identical to
        // the pending one → confirmed, commit both pairs.
        let pending = vec![candidate("rahndom", "random"), candidate("Jon", "John")];
        match tick_for(
            "send rahndom to Jon",
            FocusRead::Text("send random to John".into()),
            Some("send random to John "),
            pending.clone(),
        ) {
            TickDecision::Commit(committed) => assert_eq!(committed, pending),
            other => panic!("expected commit, got {other:?}"),
        }
    }

    #[test]
    fn a_changed_candidate_set_resets_confirmation() {
        // Only one word was fixed on the previous read; the second fix appears
        // now, so the set changed and must await a fresh confirming read.
        match tick_for(
            "send rahndom to Jon",
            FocusRead::Text("send random to John".into()),
            Some("send rahndom to John"),
            vec![candidate("Jon", "John")],
        ) {
            TickDecision::Continue {
                last_candidates,
                no_candidate: None,
                ..
            } => assert_eq!(
                last_candidates,
                vec![candidate("rahndom", "random"), candidate("Jon", "John")]
            ),
            other => panic!("expected a pending continue, got {other:?}"),
        }
    }

    #[test]
    fn changed_unlearnable_edit_reports_no_candidate() {
        // A related edit the differ cannot gate (an unrelated one-word rewrite)
        // surfaces the run/gate breakdown for the session's diagnostic log.
        match tick(
            FocusRead::Text("send it to Zurich".into()),
            Some("send it to Jon"),
            Vec::new(),
        ) {
            TickDecision::Continue {
                last_candidates,
                no_candidate: Some(diag),
                ..
            } => {
                assert!(last_candidates.is_empty());
                assert_eq!(diag.runs, 1);
                assert_eq!(diag.gated_out, 1);
            }
            other => panic!("expected a no-candidate continue, got {other:?}"),
        }
    }

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

    #[test]
    fn coalesce_delay_waits_out_the_remaining_debounce() {
        // A wakeup 30 ms after the last read waits the remaining 70 ms.
        assert_eq!(
            coalesce_delay(Some(Duration::from_millis(30)), Duration::from_millis(100)),
            Duration::from_millis(70)
        );
    }

    #[test]
    fn coalesce_delay_is_zero_once_debounce_has_passed() {
        assert_eq!(
            coalesce_delay(Some(Duration::from_millis(250)), Duration::from_millis(100)),
            Duration::ZERO
        );
    }

    #[test]
    fn coalesce_delay_is_zero_on_the_first_read() {
        // No prior read (a plain poll tick, or the first wakeup) reads at once.
        assert_eq!(
            coalesce_delay(None, Duration::from_millis(100)),
            Duration::ZERO
        );
    }
}
