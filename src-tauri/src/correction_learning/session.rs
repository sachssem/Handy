//! Post-paste learning session — the live capture stage (fork feature:
//! voice-control).
//!
//! After a transcription is pasted, [`begin_session`] opens a short learning
//! window on the app that was pasted into. The session thread pins the focused
//! element, **anchors** the pasted text inside the field (its byte span in the
//! first read that contains it, disambiguated by the caret), then diffs every
//! later read against that anchored snapshot. Only edits inside the pasted span
//! count, so text typed before/after the paste or edits elsewhere in the field
//! never feed the learner ([`differ::extract_anchored`]).
//!
//! ## Wake-ups
//!
//! The field is read once right after the session's `AXObserver` is
//! registered, so the anchor is taken from the clean pasted state. After that
//! the observer wakes the session on every value change and the field is read
//! at once (a read delayed until after a fast fix-then-Enter would only see the
//! cleared field); a `POLL_INTERVAL` timer is the fallback for apps
//! without reliable notifications.
//!
//! ## Settling and committing
//!
//! A gated candidate set is *pending* until it has been stable for [`SETTLE`]
//! with the caret off the end of a word (or [`LONG_SETTLE`] regardless — a
//! mid-word pause must not commit `Jo` for `John`). A settled set is learned
//! and the session keeps watching until the window expires, re-anchored on the
//! fixed text, so a second fix in the same window is learned too.
//!
//! When focus moves on or the field is cleared / turns unrelated
//! (Slack-style fix-then-Enter), the session ends: the pending set commits only
//! if it was a finished edit — the caret had left the word, or (also for apps
//! without caret information) it was stable for [`SETTLE`]. A half-typed word
//! (`Jon → Joh`, Enter pressed before the last keystroke was read) is dropped.
//! The window expiring or a newer paste commits what is pending; a secure field
//! or a quit app drops it.
//!
//! Commit hands the set to the store ([`super::store`]): new pairs become
//! suggestions, repeats promote, reverts block. A toast announces suggestions
//! (Accept / Never) and promotions (Undo).
//!
//! Only macOS has the Accessibility read; elsewhere [`begin_session`] is a
//! no-op.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use crate::correction_learning::ax_reader::FocusRead;
use crate::correction_learning::differ::{self, Candidate, Gate, GateProfile, PhoneticLang};
use crate::correction_learning::{CorrectionStatus, LearnedCorrectionEvent};
use std::ops::Range;
use std::time::{Duration, Instant};

/// Stable for this long (caret off a word end) → commit.
const SETTLE: Duration = Duration::from_millis(800);
/// Stable for this long → commit even with the caret at a word end.
const LONG_SETTLE: Duration = Duration::from_secs(3);
const RELATED_WORD_RATIO: f64 = 0.5;

/// Whether `current` field text still looks like an edited copy of the pasted
/// `original`. Cleared or replaced fields are unrelated.
fn is_related_to_snapshot(original: &str, current: &str) -> bool {
    let original_words = differ::normalized_words(original);
    if original_words.is_empty() {
        return false;
    }
    let current_words = differ::normalized_words(current);
    if current_words.is_empty() {
        return false;
    }
    if contains_word_run(&current_words, &original_words)
        || contains_word_run(&original_words, &current_words)
    {
        return true;
    }
    let surviving = original_words
        .iter()
        .filter(|word| current_words.contains(word))
        .count();
    (surviving as f64) / (original_words.len() as f64) >= RELATED_WORD_RATIO
}

fn contains_word_run(haystack: &[String], needle: &[String]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Whether the field text around an exactly anchored paste survived in
/// `current` with something left where the paste was. Word overlap cannot
/// judge a short paste (`Cubernetes → Kubernetes` shares no word), but intact
/// surroundings show the user edited the paste in place.
fn surroundings_survive(base: &str, paste: &Range<usize>, current: &str) -> bool {
    let before = base[..paste.start].trim_end();
    let after = base[paste.end..].trim_start();
    let Some(rest) = current.strip_prefix(before) else {
        return false;
    };
    let Some(middle) = rest.strip_suffix(after) else {
        return false;
    };
    middle.chars().any(char::is_alphanumeric)
}

/// Convert a UTF-16 offset (what AX reports) into a byte offset of `text`.
fn utf16_to_byte(text: &str, utf16: usize) -> Option<usize> {
    let mut units = 0;
    for (idx, c) in text.char_indices() {
        if units >= utf16 {
            return Some(idx);
        }
        units += c.len_utf16();
    }
    (units >= utf16).then_some(text.len())
}

/// Locate the pasted text in a field read: the occurrence whose end is nearest
/// the caret (right after a paste the caret sits at its end), else the last
/// one. Clipboard pastes may add a trailing space, so the paste is trimmed.
fn locate_paste(field: &str, pasted: &str, caret: Option<usize>) -> Option<Range<usize>> {
    let needle = pasted.trim();
    if needle.is_empty() {
        return None;
    }
    let spans: Vec<Range<usize>> = field
        .match_indices(needle)
        .map(|(start, m)| start..start + m.len())
        .collect();
    match caret {
        Some(caret) => spans
            .into_iter()
            .min_by_key(|span| span.end.abs_diff(caret)),
        None => spans.into_iter().last(),
    }
}

/// Map a byte span of `base` onto `current` across one edit, using the shared
/// prefix and suffix: ends outside the changed region keep their place, ends
/// inside it snap to the changed region's bounds in `current`.
fn remap_span(base: &str, span: &Range<usize>, current: &str) -> Range<usize> {
    let (prefix, suffix) = differ::common_affixes(base, current);
    let base_tail = base.len() - suffix;
    let current_tail = current.len() - suffix;
    // Text inserted exactly at a span end lies outside the span: the start
    // prefers the suffix mapping, the end the prefix mapping.
    let start = if span.start >= base_tail {
        span.start - base_tail + current_tail
    } else {
        span.start.min(prefix)
    };
    let end = if span.end <= prefix {
        span.end
    } else if span.end >= base_tail {
        span.end - base_tail + current_tail
    } else {
        current_tail
    }
    .max(start);
    start..end
}

/// Whether the caret sits right after a word character — the user may still
/// be typing that word.
fn caret_in_word(text: &str, caret: Option<usize>) -> bool {
    let Some(caret) = caret else {
        return false;
    };
    let before = text[..caret.min(text.len())].chars().next_back();
    let after = text.get(caret..).and_then(|rest| rest.chars().next());
    before.is_some_and(char::is_alphanumeric) && !after.is_some_and(char::is_alphanumeric)
}

/// The field state the session diffs against.
#[derive(Debug, Clone)]
struct Anchor {
    base: String,
    paste: Range<usize>,
    /// `false` while only the pasted text itself serves as base (the paste was
    /// not found verbatim in the field yet); upgraded once it is.
    exact: bool,
}

impl Anchor {
    /// The (possibly already fixed) pasted text.
    fn paste_text(&self) -> &str {
        &self.base[self.paste.clone()]
    }

    /// Whether a field read is still an edited copy of the paste.
    fn related(&self, current: &str) -> bool {
        is_related_to_snapshot(self.paste_text(), current)
            || (self.exact && surroundings_survive(&self.base, &self.paste, current))
    }

    /// Re-anchor on `current` after `learned` was committed, so the session
    /// keeps watching the fixed text without learning the same edit again.
    fn rebase(&mut self, current: &str, caret: Option<usize>, learned: &[Candidate]) {
        let mut expected = self.paste_text().to_string();
        for candidate in learned {
            expected = expected.replacen(&candidate.misheard, &candidate.intended, 1);
        }
        if let Some(span) = locate_paste(current, &expected, caret) {
            *self = Anchor {
                base: current.to_string(),
                paste: span,
                exact: true,
            };
        } else if self.exact {
            self.paste = remap_span(&self.base, &self.paste, current);
            self.base = current.to_string();
        } else {
            self.paste = 0..expected.len();
            self.base = expected;
        }
    }
}

/// Read-only inputs of every tick.
struct TickCtx<'a> {
    pasted: &'a str,
    profile: &'a GateProfile,
    lang: PhoneticLang,
    /// Lowercased pairs the apply stage fires (see
    /// [`differ::extract_anchored`]).
    applied: &'a [(String, String)],
}

/// Mutable per-session state, kept apart from the driver so the decision
/// logic is pure and unit-testable.
#[derive(Debug, Default)]
struct SessionState {
    anchor: Option<Anchor>,
    last_text: Option<String>,
    last_caret: Option<usize>,
    pending: Vec<Candidate>,
    pending_since: Option<Instant>,
    unrelated_logged: bool,
    oversized_logged: bool,
    /// Diagnostics of the last tick, drained by the driver for logging.
    notes: Vec<Note>,
}

/// What one tick decided.
#[derive(Debug, PartialEq)]
enum Decision {
    /// End the session without learning.
    Teardown(&'static str),
    /// Learn this set, then end the session.
    Commit(Vec<Candidate>, &'static str),
    /// Learn this settled set and keep watching.
    Learn(Vec<Candidate>),
    Continue,
}

/// Content-free notes the driver logs at debug level.
#[derive(Debug, PartialEq)]
enum Note {
    Anchored { exact: bool },
    Unrelated,
    Rejected(Vec<Gate>),
    Reformulation(usize),
    Oversized,
    Pending(usize),
    PendingDropped,
}

impl SessionState {
    /// Whether the last read had the caret right after a word character.
    fn typing(&self) -> bool {
        self.last_text
            .as_deref()
            .is_some_and(|text| caret_in_word(text, self.last_caret))
    }

    /// How long the pending set has been stable by `now`.
    fn pending_age(&self, now: Instant) -> Duration {
        self.pending_since
            .map_or(Duration::ZERO, |since| now.saturating_duration_since(since))
    }

    /// The pending set, if it has settled by `now`.
    fn settled(&self, now: Instant) -> bool {
        if self.pending.is_empty() || self.pending_since.is_none() {
            return false;
        }
        let stable = self.pending_age(now);
        stable >= LONG_SETTLE || (stable >= SETTLE && !self.typing())
    }

    /// Whether the pending set is a finished edit, judged when the field is
    /// cleared or focus moves on: the caret had left the word, or the set was
    /// stable for [`SETTLE`] (the only evidence for apps without a caret).
    fn pending_finished(&self, now: Instant) -> bool {
        (self.last_caret.is_some() && !self.typing()) || self.pending_age(now) >= SETTLE
    }

    /// When the pending set will settle, for the driver's wait timeout.
    fn settle_deadline(&self) -> Option<Instant> {
        let since = self.pending_since?;
        if self.pending.is_empty() {
            return None;
        }
        Some(since + if self.typing() { LONG_SETTLE } else { SETTLE })
    }

    /// Commit whatever is pending (final edit state reached).
    fn flush(&mut self, reason: &'static str) -> Decision {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            Decision::Teardown(reason)
        } else {
            Decision::Commit(pending, reason)
        }
    }

    /// End the session on a clear or focus change: commit the pending set if it
    /// was a finished edit, drop it otherwise.
    fn finish_edit(&mut self, reason: &'static str, now: Instant) -> Decision {
        if self.pending.is_empty() || self.pending_finished(now) {
            return self.flush(reason);
        }
        self.pending.clear();
        self.pending_since = None;
        self.notes.push(Note::PendingDropped);
        Decision::Teardown(reason)
    }

    /// Stop learning without committing even a finished pending edit.
    fn cancel(&mut self) -> Decision {
        if !self.pending.is_empty() {
            self.notes.push(Note::PendingDropped);
        }
        self.pending.clear();
        self.pending_since = None;
        Decision::Teardown("learning disabled")
    }

    /// Learn the settled set and re-anchor on the text it settled in.
    fn learn_settled(&mut self) -> Decision {
        let pending = std::mem::take(&mut self.pending);
        self.pending_since = None;
        if let (Some(anchor), Some(text)) = (self.anchor.as_mut(), self.last_text.as_deref()) {
            anchor.rebase(text, self.last_caret, &pending);
        }
        Decision::Learn(pending)
    }

    /// Process one field read. `caret` is a byte offset into the read text.
    fn tick(
        &mut self,
        read: FocusRead,
        caret: Option<usize>,
        now: Instant,
        ctx: &TickCtx,
    ) -> Decision {
        let current = match read {
            FocusRead::Secure => return Decision::Teardown("secure field"),
            FocusRead::AppGone => return Decision::Teardown("app gone"),
            FocusRead::FocusChanged => return self.finish_edit("focus moved", now),
            // A transient AX hiccup: keep what is pending, maybe settle.
            FocusRead::NoSignal => {
                return if self.settled(now) {
                    self.learn_settled()
                } else {
                    Decision::Continue
                };
            }
            FocusRead::Text(text) => text,
        };

        // Cap work before locating the paste or comparing whole-field words.
        if current.chars().nth(differ::MAX_FIELD_CHARS).is_some() {
            if !self.oversized_logged {
                self.oversized_logged = true;
                self.notes.push(Note::Oversized);
            }
            if !self.pending.is_empty() {
                self.notes.push(Note::PendingDropped);
            }
            self.pending.clear();
            self.pending_since = None;
            return Decision::Continue;
        }

        // Anchor (or upgrade a provisional anchor) on the first read that
        // contains the paste verbatim.
        if !self.anchor.as_ref().is_some_and(|a| a.exact) {
            if let Some(span) = locate_paste(&current, ctx.pasted, caret) {
                self.anchor = Some(Anchor {
                    base: current.clone(),
                    paste: span,
                    exact: true,
                });
                self.notes.push(Note::Anchored { exact: true });
            } else if self.anchor.is_none() && is_related_to_snapshot(ctx.pasted, &current) {
                // Already edited before the first read (or a typed paste still
                // landing): diff against the pasted text itself; the differ's
                // edge handling keeps surrounding text out.
                let base = ctx.pasted.trim().to_string();
                let paste = 0..base.len();
                self.anchor = Some(Anchor {
                    base,
                    paste,
                    exact: false,
                });
                self.notes.push(Note::Anchored { exact: false });
            }
        }

        if self.last_text.as_deref() == Some(current.as_str()) {
            self.last_caret = caret;
            return if self.settled(now) {
                self.learn_settled()
            } else {
                Decision::Continue
            };
        }

        let Some(anchor) = &self.anchor else {
            self.last_text = Some(current);
            self.last_caret = caret;
            return Decision::Continue;
        };

        if !anchor.related(&current) {
            // Cleared on submit, or replaced: the last related state was final
            // — judged on that state's caret, so before recording this read.
            if current.trim().is_empty() || !self.pending.is_empty() {
                return self.finish_edit("field cleared or unrelated", now);
            }
            self.last_text = Some(current);
            self.last_caret = caret;
            if !self.unrelated_logged {
                self.unrelated_logged = true;
                self.notes.push(Note::Unrelated);
            }
            return Decision::Continue;
        }
        self.unrelated_logged = false;

        let extraction = differ::extract_anchored(
            &anchor.base,
            anchor.paste.clone(),
            &current,
            ctx.profile,
            ctx.lang,
            ctx.applied,
        );
        self.last_text = Some(current);
        self.last_caret = caret;
        if extraction.oversized && !self.oversized_logged {
            self.oversized_logged = true;
            self.notes.push(Note::Oversized);
        }
        if extraction.reformulation {
            self.notes.push(Note::Reformulation(extraction.runs));
        }
        if !extraction.rejected.is_empty() {
            self.notes.push(Note::Rejected(extraction.rejected.clone()));
        }

        if extraction.candidates.is_empty() {
            if !self.pending.is_empty() {
                self.notes.push(Note::PendingDropped);
            }
            self.pending.clear();
            self.pending_since = None;
            return Decision::Continue;
        }
        if extraction.candidates != self.pending {
            self.notes.push(Note::Pending(extraction.candidates.len()));
            self.pending = extraction.candidates;
            self.pending_since = Some(now);
        }
        if self.settled(now) {
            return self.learn_settled();
        }
        Decision::Continue
    }
}

/// The toast event for one commit's new suggestions and promotions, ids split
/// by status so Accept / Never act on the suggestions only and Undo on the
/// promoted pairs only. The first pair (suggestions first: they ask for a
/// decision) is shown verbatim. `None` when nothing is worth a toast.
fn toast_event(
    entries: &[(String, Candidate, CorrectionStatus)],
) -> Option<LearnedCorrectionEvent> {
    let ids = |status: CorrectionStatus| -> Vec<String> {
        entries
            .iter()
            .filter(|(_, _, s)| *s == status)
            .map(|(id, _, _)| id.clone())
            .collect()
    };
    let (first_id, first, status) = entries
        .iter()
        .find(|(_, _, s)| *s == CorrectionStatus::Suggested)
        .or_else(|| entries.first())?;
    Some(LearnedCorrectionEvent {
        id: first_id.clone(),
        misheard: first.misheard.clone(),
        intended: first.intended.clone(),
        status: *status,
        suggested_ids: ids(CorrectionStatus::Suggested),
        active_ids: ids(CorrectionStatus::Active),
        extra: (entries.len() - 1) as u32,
    })
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{toast_event, Decision, Note, SessionState, TickCtx};
    use crate::correction_learning::ax_reader;
    use crate::correction_learning::differ::{Candidate, GateProfile, PhoneticLang};
    use crate::correction_learning::last_transcription_language;
    use crate::correction_learning::store::{self, CorrectionStatus, Observation};
    use crate::journal::{LearningPair, LearningRecord};
    use crate::settings::{self, AppSettings, PasteMethod};
    use log::{debug, info};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, Instant};
    use tauri::AppHandle;
    use tauri_specta::Event;

    /// Fallback re-read interval for apps without reliable AX notifications.
    const POLL_INTERVAL: Duration = Duration::from_secs(2);
    /// Shortest timer wait (a settle deadline that is due now still yields).
    const MIN_WAIT: Duration = Duration::from_millis(100);
    /// Minimum spacing between two notification-driven reads, so an app
    /// spamming value changes cannot spin the session thread. Far below
    /// keystroke spacing: every keystroke still gets its own read.
    const MIN_READ_GAP: Duration = Duration::from_millis(15);
    const MIN_WINDOW_SECS: u32 = 10;
    const MAX_WINDOW_SECS: u32 = 300;

    /// Bumped on every `begin_session`; a running session ends once its
    /// generation is no longer current.
    static GENERATION: AtomicU64 = AtomicU64::new(0);
    /// Disabling either switch invalidates pending edits even if re-enabled
    /// before the watcher wakes up. Normal supersession still flushes edits.
    static CANCELLATION_GENERATION: AtomicU64 = AtomicU64::new(0);

    fn window(settings: &AppSettings) -> Duration {
        let secs = settings
            .learn_corrections_window_secs
            .clamp(MIN_WINDOW_SECS, MAX_WINDOW_SECS);
        Duration::from_secs(secs as u64)
    }

    struct SessionParams {
        window: Duration,
        cancellation_generation: u64,
        profile: GateProfile,
        lang: PhoneticLang,
        /// Base code of the transcription's language, tagged on learned pairs.
        lang_code: Option<String>,
        /// Dictation whose paste opened the session (journal join key).
        dictation_id: Option<u64>,
        /// Pairs the apply stage fires, so a revert is recognised.
        applied: Vec<(String, String)>,
    }

    /// Journal a session that never ran.
    fn journal_skip(dictation_id: Option<u64>, reason: &str) {
        crate::journal::record_learning(LearningRecord {
            id: dictation_id,
            skipped: Some(reason.to_string()),
            ..Default::default()
        });
    }

    /// Snapshot a just-pasted transcription and open the learning window on the
    /// app it was pasted into. Runs on the main thread (the paste callsite) only
    /// long enough to resolve the frontmost pid; everything else, including the
    /// AX element snapshot (which may wait on Electron's tree), runs off-thread.
    pub fn begin_session(app: &AppHandle, original: String, dictation_id: Option<u64>) {
        let cancellation_generation = CANCELLATION_GENERATION.load(Ordering::SeqCst);
        let settings = settings::get_settings(app);
        if !settings.learn_corrections_enabled || !settings.learn_from_edits_enabled {
            return;
        }
        if matches!(
            settings.paste_method,
            PasteMethod::None | PasteMethod::ExternalScript
        ) {
            debug!(
                "learn: no session — paste method {:?} leaves no editable field",
                settings.paste_method
            );
            journal_skip(dictation_id, "paste method leaves no editable field");
            return;
        }
        let Some(pid) = ax_reader::frontmost_pid() else {
            debug!("learn: no session — no frontmost pid resolved");
            journal_skip(dictation_id, "no frontmost pid");
            return;
        };
        let lang_code = last_transcription_language();
        let params = SessionParams {
            window: window(&settings),
            cancellation_generation,
            profile: GateProfile::for_aggressiveness(settings.learn_corrections_aggressiveness),
            lang: PhoneticLang::for_code(lang_code.as_deref()),
            lang_code,
            dictation_id,
            applied: store::snapshot().applied_pairs(),
        };
        let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        let app = app.clone();
        std::thread::spawn(move || run_session(app, generation, pid, original, params));
    }

    /// End the running session (if any) without starting a new one. It
    /// flushes what it observed so far as `superseded` on its next wake-up,
    /// before reading the field again.
    pub fn end_session() {
        GENERATION.fetch_add(1, Ordering::SeqCst);
    }

    /// Cancel an active session and discard all uncommitted candidates.
    pub fn cancel_session() {
        CANCELLATION_GENERATION.fetch_add(1, Ordering::SeqCst);
        end_session();
    }

    fn run_session(
        app: AppHandle,
        generation: u64,
        pid: i32,
        original: String,
        params: SessionParams,
    ) {
        let app_name = ax_reader::process_name(pid);
        let mut journal = LearningRecord {
            id: params.dictation_id,
            session: generation,
            app: app_name.clone(),
            lang: params.lang_code.clone(),
            window_secs: params.window.as_secs(),
            ..Default::default()
        };
        let Some(focus) = ax_reader::snapshot_focused_element(pid) else {
            debug!(
                "learn: no session — focused-element snapshot failed for {} (AX permission?)",
                app_name.as_deref().unwrap_or("?")
            );
            journal.skipped = Some("focused-element snapshot failed".to_string());
            crate::journal::record_learning(journal);
            return;
        };
        let observer = ax_reader::create_value_change_observer(pid, &focus);
        journal.observer = observer.is_some();
        // Bundle id and sizes only — never the pasted text.
        debug!(
            "learn: session {} started — app {}, {} pasted chars, lang {}, window {}s, {}",
            generation,
            app_name.as_deref().unwrap_or("?"),
            original.chars().count(),
            params.lang_code.as_deref().unwrap_or("?"),
            params.window.as_secs(),
            if observer.is_some() {
                "AX observer"
            } else {
                "polling"
            }
        );

        let started = Instant::now();
        let mut state = SessionState::default();
        let ctx = TickCtx {
            pasted: &original,
            profile: &params.profile,
            lang: params.lang,
            applied: &params.applied,
        };
        let mut last_read: Option<Instant> = None;
        // Hash of the previous read's text, to count observed edits.
        let mut last_text_hash: Option<u64> = None;

        loop {
            // The first read happens at once, so the anchor is taken from the
            // clean pasted state. After that, wake on a value change, the poll
            // interval, or the pending set's settle deadline — whichever is
            // first.
            if let Some(at) = last_read {
                let now = Instant::now();
                let timeout = state
                    .settle_deadline()
                    .map(|deadline| deadline.saturating_duration_since(now))
                    .map_or(POLL_INTERVAL, |until| until.clamp(MIN_WAIT, POLL_INTERVAL));
                match &observer {
                    // A value change is read at once (no debounce sleep): a
                    // fix-then-Enter clears the field within ~100 ms, and a
                    // delayed read would only see the cleared field.
                    Some(observer) => {
                        if observer.wait(timeout) {
                            let since = at.elapsed();
                            if since < MIN_READ_GAP {
                                std::thread::sleep(MIN_READ_GAP - since);
                            }
                        }
                    }
                    None => std::thread::sleep(timeout),
                }
            }

            if CANCELLATION_GENERATION.load(Ordering::SeqCst) != params.cancellation_generation {
                let decision = state.cancel();
                for note in std::mem::take(&mut state.notes) {
                    log_note(generation, note, &mut journal);
                }
                finish(&app, generation, decision, &params, journal, started);
                return;
            }
            if GENERATION.load(Ordering::SeqCst) != generation {
                let decision = state.flush("superseded");
                finish(&app, generation, decision, &params, journal, started);
                return;
            }
            if started.elapsed() >= params.window {
                let decision = state.flush("window elapsed");
                finish(&app, generation, decision, &params, journal, started);
                return;
            }
            last_read = Some(Instant::now());

            let read = ax_reader::read_focused(pid, app_name.as_deref(), &focus);
            journal.reads += 1;
            let caret = match &read {
                ax_reader::FocusRead::Text(text) => {
                    let hash = text_hash(text);
                    if last_text_hash.is_some_and(|last| last != hash) {
                        journal.edits_observed += 1;
                    }
                    last_text_hash = Some(hash);
                    ax_reader::read_caret(&focus)
                        .and_then(|utf16| super::utf16_to_byte(text, utf16))
                }
                _ => None,
            };
            let decision = if CANCELLATION_GENERATION.load(Ordering::SeqCst)
                != params.cancellation_generation
            {
                state.cancel()
            } else {
                state.tick(read, caret, Instant::now(), &ctx)
            };
            for note in std::mem::take(&mut state.notes) {
                log_note(generation, note, &mut journal);
            }
            match decision {
                Decision::Continue => {}
                // A settled fix: learn it and keep watching for the next one.
                Decision::Learn(candidates) => {
                    debug!(
                        "learn: session {} committing {} candidate(s) — settled, still watching",
                        generation,
                        candidates.len()
                    );
                    let pairs = commit(&app, candidates, params.lang_code.as_deref());
                    journal.committed.extend(pairs);
                }
                decision => {
                    finish(&app, generation, decision, &params, journal, started);
                    return;
                }
            }
        }
    }

    fn text_hash(text: &str) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut hasher);
        hasher.finish()
    }

    /// Count a note into the journal record, then log it.
    fn tally_note(note: &Note, journal: &mut LearningRecord) {
        match note {
            Note::Anchored { exact } => {
                journal.anchored = Some(if *exact { "exact" } else { "paste_only" }.to_string())
            }
            Note::Unrelated => journal.unrelated += 1,
            Note::Rejected(gates) => {
                for gate in gates {
                    *journal
                        .gate_rejections
                        .entry(gate.name().to_string())
                        .or_default() += 1;
                }
            }
            Note::Reformulation(_) => journal.reformulations += 1,
            Note::Oversized => journal.oversized += 1,
            Note::Pending(_) => journal.pending_sets += 1,
            Note::PendingDropped => journal.pending_dropped += 1,
        }
    }

    fn log_note(generation: u64, note: Note, journal: &mut LearningRecord) {
        tally_note(&note, journal);
        match note {
            Note::Anchored { exact } => debug!(
                "learn: session {} anchored the paste ({})",
                generation,
                if exact {
                    "in field"
                } else {
                    "paste-only fallback"
                }
            ),
            Note::Unrelated => debug!(
                "learn: session {} field turned unrelated to the paste",
                generation
            ),
            Note::Rejected(gates) => {
                let names: Vec<&str> = gates.iter().map(|gate| gate.name()).collect();
                debug!(
                    "learn: session {} candidate(s) rejected by gate(s) {}",
                    generation,
                    names.join(", ")
                );
            }
            Note::Reformulation(runs) => debug!(
                "learn: session {} edit has {} change runs — reformulation, nothing learned",
                generation, runs
            ),
            Note::Oversized => debug!(
                "learn: session {} field or edit too large to diff",
                generation
            ),
            Note::Pending(count) => debug!(
                "learn: session {} {} candidate(s) pending until settled",
                generation, count
            ),
            Note::PendingDropped => debug!(
                "learn: session {} pending candidate(s) dropped by a later edit",
                generation
            ),
        }
    }

    fn finish(
        app: &AppHandle,
        generation: u64,
        decision: Decision,
        params: &SessionParams,
        mut journal: LearningRecord,
        started: Instant,
    ) {
        match decision {
            Decision::Teardown(reason) => {
                debug!("learn: session {} ended — {}", generation, reason);
                journal.end_reason = Some(reason.to_string());
            }
            Decision::Commit(candidates, reason) => {
                debug!(
                    "learn: session {} committing {} candidate(s) — {}",
                    generation,
                    candidates.len(),
                    reason
                );
                journal.end_reason = Some(reason.to_string());
                let pairs = commit(app, candidates, params.lang_code.as_deref());
                journal.committed.extend(pairs);
            }
            Decision::Learn(_) | Decision::Continue => {}
        }
        journal.duration_ms = started.elapsed().as_millis() as u64;
        crate::journal::record_learning(journal);
    }

    /// Hand a settled set to the store and toast what changed. The pair text is
    /// user content: info logs carry ids and lengths only, verbatim pairs go to
    /// debug. Returns every pair's outcome for the journal.
    fn commit(
        app: &AppHandle,
        candidates: Vec<Candidate>,
        lang: Option<&str>,
    ) -> Vec<LearningPair> {
        let now = chrono::Utc::now().timestamp();
        let outcomes: Vec<(Candidate, Observation)> = store::update(app, |data| {
            let outcomes: Vec<(Candidate, Observation)> = candidates
                .into_iter()
                .map(|candidate| {
                    let outcome = data.observe(&candidate, lang, now);
                    (candidate, outcome)
                })
                .collect();
            let changed = outcomes.iter().any(|(_, outcome)| outcome.mutates_store());
            (outcomes, changed)
        });

        let mut toast: Vec<(String, Candidate, CorrectionStatus)> = Vec::new();
        let mut journal_pairs = Vec::with_capacity(outcomes.len());
        for (candidate, outcome) in outcomes {
            let label = outcome.label();
            let id = match &outcome {
                Observation::Suggested(id)
                | Observation::Promoted(id)
                | Observation::Reobserved(id)
                | Observation::InverseReverted(id) => Some(id),
                _ => None,
            };
            info!(
                "learned correction {} {}: misheard {} chars -> intended {} chars",
                id.map(String::as_str).unwrap_or("-"),
                label,
                candidate.misheard.chars().count(),
                candidate.intended.chars().count()
            );
            debug!(
                "learned correction {} (verbatim): {} -> {}",
                label, candidate.misheard, candidate.intended
            );
            journal_pairs.push(LearningPair {
                misheard: candidate.misheard.clone(),
                intended: candidate.intended.clone(),
                outcome: label.to_string(),
                pair_id: id.cloned(),
            });
            match outcome {
                Observation::Suggested(id) => {
                    toast.push((id, candidate, CorrectionStatus::Suggested))
                }
                Observation::Promoted(id) => toast.push((id, candidate, CorrectionStatus::Active)),
                _ => {}
            }
        }

        let Some(event) = toast_event(&toast) else {
            return journal_pairs;
        };
        crate::correction_learning::toast::set_pending_learned_toast(event.clone());
        if let Err(err) = event.emit(app) {
            log::error!("Failed to emit learned-correction event: {}", err);
        }
        crate::correction_learning::toast::show_learned_toast(app, event);
        journal_pairs
    }
}

#[cfg(target_os = "macos")]
pub use imp::{begin_session, cancel_session, end_session};

/// No-op on platforms without an Accessibility read — the feature is silently
/// absent there (the deterministic apply stage still works everywhere).
#[cfg(not(target_os = "macos"))]
pub fn begin_session(_app: &tauri::AppHandle, _original: String, _dictation_id: Option<u64>) {}

#[cfg(not(target_os = "macos"))]
pub fn end_session() {}

#[cfg(not(target_os = "macos"))]
pub fn cancel_session() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_learning::differ::Aggressiveness;

    fn profile() -> GateProfile {
        GateProfile::for_aggressiveness(Aggressiveness::Balanced)
    }

    struct Harness {
        state: SessionState,
        pasted: &'static str,
        applied: Vec<(String, String)>,
        t0: Instant,
    }

    impl Harness {
        fn new(pasted: &'static str) -> Self {
            Harness {
                state: SessionState::default(),
                pasted,
                applied: Vec::new(),
                t0: Instant::now(),
            }
        }

        fn read(&mut self, read: FocusRead, caret: Option<usize>, at_ms: u64) -> Decision {
            let profile = profile();
            let ctx = TickCtx {
                pasted: self.pasted,
                profile: &profile,
                lang: PhoneticLang::Other,
                applied: &self.applied,
            };
            self.state
                .tick(read, caret, self.t0 + Duration::from_millis(at_ms), &ctx)
        }

        /// A read without caret information (as from apps exposing none).
        fn text(&mut self, text: &str, at_ms: u64) -> Decision {
            self.read(FocusRead::Text(text.to_string()), None, at_ms)
        }

        /// A read with the caret at byte `caret`.
        fn at(&mut self, text: &str, caret: usize, at_ms: u64) -> Decision {
            self.read(FocusRead::Text(text.to_string()), Some(caret), at_ms)
        }
    }

    /// The intended spans of a commit (final or settled-and-continue).
    fn intended(decision: &Decision) -> Vec<&str> {
        match decision {
            Decision::Commit(candidates, _) | Decision::Learn(candidates) => {
                candidates.iter().map(|c| c.intended.as_str()).collect()
            }
            other => panic!("expected a commit, got {other:?}"),
        }
    }

    #[test]
    fn oversized_reads_skip_anchoring_and_log_once() {
        let mut h = Harness::new("ask Jon now");
        let oversized = format!("ask Jon now{}", "ä".repeat(differ::MAX_FIELD_CHARS));
        assert_eq!(h.text(&oversized, 0), Decision::Continue);
        assert!(h.state.anchor.is_none());
        assert!(h.state.last_text.is_none());
        assert_eq!(h.state.notes, vec![Note::Oversized]);
        h.state.notes.clear();
        assert_eq!(h.text(&oversized, 1_000), Decision::Continue);
        assert!(h.state.notes.is_empty());
        assert_eq!(h.text("ask Jon now", 2_000), Decision::Continue);
        assert!(h.state.anchor.is_some());
    }

    #[test]
    fn oversized_read_drops_pending_before_it_can_settle() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.text("ask John now", 100);
        assert!(!h.state.pending.is_empty());
        h.state.notes.clear();
        let oversized = "x".repeat(differ::MAX_FIELD_CHARS + 1);
        assert_eq!(h.text(&oversized, 1_000), Decision::Continue);
        assert_eq!(h.state.notes, vec![Note::Oversized, Note::PendingDropped]);
        assert!(h.state.pending.is_empty());
        assert!(h.state.pending_since.is_none());
        assert_eq!(h.read(FocusRead::NoSignal, None, 4_000), Decision::Continue);
    }

    #[test]
    fn field_size_cap_counts_characters_and_allows_the_limit() {
        let mut h = Harness::new("ask Jon now");
        let field = format!(
            "ask Jon now{} ",
            "ä".repeat(differ::MAX_FIELD_CHARS - "ask Jon now ".chars().count())
        );
        assert_eq!(h.text(&field, 0), Decision::Continue);
        assert!(h.state.anchor.as_ref().is_some_and(|anchor| anchor.exact));
        assert!(!h.state.notes.contains(&Note::Oversized));
    }

    #[test]
    fn settles_after_stability_without_a_second_poll() {
        let mut h = Harness::new("deploy on Cubernetes");
        assert_eq!(h.text("deploy on Cubernetes", 0), Decision::Continue);
        assert_eq!(h.text("deploy on Kubernetes ", 100), Decision::Continue);
        // A timer wake with unchanged text after SETTLE commits.
        let d = h.text("deploy on Kubernetes ", 1000);
        assert_eq!(intended(&d), vec!["Kubernetes"]);
    }

    #[test]
    fn clear_after_a_settled_fix_ends_the_session() {
        let mut h = Harness::new("ask Jon about k8s");
        h.text("ask Jon about k8s", 0);
        h.text("ask John about k8s", 100);
        assert_eq!(intended(&h.text("ask John about k8s", 1_000)), vec!["John"]);
        assert!(h.state.pending.is_empty());
        assert_eq!(
            h.text("", 1_100),
            Decision::Teardown("field cleared or unrelated")
        );
    }

    #[test]
    fn clear_without_any_correction_ends_an_anchored_session() {
        let mut h = Harness::new("ask Jon about k8s");
        h.text("ask Jon about k8s", 0);
        assert_eq!(
            h.text("  ", 100),
            Decision::Teardown("field cleared or unrelated")
        );
        // An initial empty read before the paste is anchored remains harmless.
        let mut h = Harness::new("ask Jon about k8s");
        assert_eq!(h.text("", 0), Decision::Continue);
    }

    #[test]
    fn disabling_learning_drops_a_finished_pending_fix() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.at("ask John now", 0, 100);
        assert!(!h.state.pending.is_empty());
        assert_eq!(h.state.cancel(), Decision::Teardown("learning disabled"));
        assert!(h.state.pending.is_empty());
        assert!(h.state.pending_since.is_none());
        assert!(h.state.notes.contains(&Note::PendingDropped));
        assert_eq!(
            h.state.flush("superseded"),
            Decision::Teardown("superseded")
        );
    }

    #[test]
    fn clear_on_submit_commits_a_finished_fix() {
        // Slack: fix the word, press Enter — the field empties before any
        // confirming read. Without caret information the fix must have been
        // stable for SETTLE.
        let mut h = Harness::new("ask Jon about k8s");
        h.text("ask Jon about k8s", 0);
        assert_eq!(h.text("ask John about k8s", 200), Decision::Continue);
        let d = h.text("", 1_000);
        assert!(matches!(d, Decision::Commit(..)));
        assert_eq!(intended(&d), vec!["John"]);

        // With the caret off the word, the fix is final right away.
        let mut h = Harness::new("ask Jon about k8s");
        h.at("ask Jon about k8s", 17, 0);
        h.at("ask John about k8s", 0, 200);
        assert_eq!(intended(&h.at("", 0, 300)), vec!["John"]);
    }

    #[test]
    fn half_typed_word_is_dropped_on_clear() {
        // `Jonn` retyped as `Joh`, then `n` and Enter within one read: the last
        // read state is a half-typed word and must not be learned. Unlike the
        // common word `Jon`, `Jonn` lets this reach the pending-edit guard.
        let mut h = Harness::new("ask Jonn about k8s");
        h.at("ask Jonn about k8s", 18, 0);
        assert_eq!(
            h.at("ask Joh about k8s", "ask Joh".len(), 200),
            Decision::Continue
        );
        assert!(matches!(h.at("", 0, 300), Decision::Teardown(_)));
        assert!(h.state.notes.contains(&Note::PendingDropped));

        // Same without caret information: too fresh to trust.
        let mut h = Harness::new("deploy on Cubernetes now");
        h.text("deploy on Cubernetes now", 0);
        h.text("deploy on Kubernete now", 200);
        assert!(matches!(h.text("", 300), Decision::Teardown(_)));
    }

    #[test]
    fn likely_typo_never_becomes_pending_or_commits_on_clear() {
        let mut h = Harness::new("Einkaufsliste: Milch Eier Brot");
        h.text("Einkaufsliste: Milch Eier Brot", 0);
        assert_eq!(
            h.text("Einkaufsliste: Mlch Eier Brot", 200),
            Decision::Continue
        );
        assert!(h.state.pending.is_empty());
        assert!(h
            .state
            .notes
            .contains(&Note::Rejected(vec![differ::Gate::LikelyTypo])));
        assert_eq!(
            h.text("Einkaufsliste: Mlch Eier Brot", 1_000),
            Decision::Continue
        );
        assert_eq!(
            h.text("", 1_100),
            Decision::Teardown("field cleared or unrelated")
        );
        assert!(h.state.pending.is_empty());
    }

    #[test]
    fn focus_change_commits_a_finished_fix_and_drops_a_fresh_one() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.text("ask John now", 100);
        assert_eq!(
            intended(&h.read(FocusRead::FocusChanged, None, 1_000)),
            vec!["John"]
        );

        let mut h = Harness::new("ask Jon now");
        h.at("ask Jon now", 11, 0);
        h.at("ask John now", "ask John".len(), 100);
        assert!(matches!(
            h.read(FocusRead::FocusChanged, None, 200),
            Decision::Teardown(_)
        ));

        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        assert!(matches!(
            h.read(FocusRead::FocusChanged, None, 100),
            Decision::Teardown(_)
        ));
    }

    #[test]
    fn secure_field_never_commits() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.text("ask John now", 100);
        assert!(matches!(
            h.read(FocusRead::Secure, None, 200),
            Decision::Teardown(_)
        ));
    }

    #[test]
    fn mid_word_pause_waits_for_a_word_boundary() {
        let mut h = Harness::new("send it to Kubernetis please");
        h.text("send it to Kubernetis please", 0);
        // Retyping the ending, paused mid-word with the caret at the end of the
        // partial word: no commit after SETTLE.
        let partial = "send it to Kubernete please";
        let caret = "send it to Kubernete".len();
        assert_eq!(h.at(partial, caret, 100), Decision::Continue);
        assert_eq!(h.at(partial, caret, 1_000), Decision::Continue);
        // The finished word: the caret still touches it, so the short settle
        // does not apply…
        let done = "send it to Kubernetes please";
        let caret = "send it to Kubernetes".len();
        h.at(done, caret, 1_100);
        assert_eq!(h.at(done, caret, 2_000), Decision::Continue);
        // …until the caret moves off the word, or LONG_SETTLE passes.
        let d = h.at(done, 0, 2_100);
        assert_eq!(intended(&d), vec!["Kubernetes"]);
    }

    #[test]
    fn text_around_the_paste_is_isolated() {
        let mut h = Harness::new("deploy on Cubernetes now");
        h.text("Hi team, deploy on Cubernetes now", 0);
        h.text("Hello team, deploy on Kubernetes now. Thanks!", 100);
        let d = h.text("", 1_000);
        assert_eq!(intended(&d), vec!["Kubernetes"]);
    }

    #[test]
    fn paste_not_yet_landed_is_anchored_later() {
        let mut h = Harness::new("deploy on Cubernetes");
        assert_eq!(h.text("Hi team, ", 0), Decision::Continue);
        h.text("Hi team, deploy on Cubernetes", 100);
        h.text("Hi team, deploy on Kubernetes", 200);
        let d = h.read(FocusRead::FocusChanged, None, 1_100);
        assert_eq!(intended(&d), vec!["Kubernetes"]);
    }

    #[test]
    fn first_read_anchors_on_the_clean_paste() {
        let mut h = Harness::new("deploy on Cubernetes");
        assert_eq!(
            h.text("Hi team, deploy on Cubernetes", 0),
            Decision::Continue
        );
        assert_eq!(h.state.notes, vec![Note::Anchored { exact: true }]);
        let anchor = h.state.anchor.as_ref().expect("anchored");
        assert_eq!(anchor.paste_text(), "deploy on Cubernetes");
    }

    #[test]
    fn one_word_paste_fix_is_related_and_learned() {
        // The whole field is the one-word paste: no word survives the fix.
        let mut h = Harness::new("Cubernetes");
        h.text("Cubernetes", 0);
        h.text("Kubernetes", 100);
        assert_eq!(intended(&h.text("Kubernetes", 1_000)), vec!["Kubernetes"]);

        // Inside other text: the surroundings survive.
        let mut h = Harness::new("Cubernetes");
        h.text("We run Cubernetes in prod", 0);
        h.text("We run Kubernetes in prod", 100);
        assert_eq!(
            intended(&h.text("We run Kubernetes in prod", 1_000)),
            vec!["Kubernetes"]
        );

        // Surroundings replaced too: unrelated, nothing learned.
        let mut h = Harness::new("Cubernetes");
        h.text("We run Cubernetes in prod", 0);
        assert_eq!(h.text("Totally new message", 100), Decision::Continue);
        assert!(h.state.notes.contains(&Note::Unrelated));
    }

    #[test]
    fn keeps_learning_after_a_settled_fix() {
        let mut h = Harness::new("ask Jon to deploy Cubernetes");
        h.text("ask Jon to deploy Cubernetes", 0);
        h.text("ask John to deploy Cubernetes", 100);
        assert!(matches!(
            h.text("ask John to deploy Cubernetes", 1_000),
            Decision::Learn(_)
        ));
        // The settled fix is not learned again; the second one is.
        assert_eq!(
            h.text("ask John to deploy Cubernetes", 2_000),
            Decision::Continue
        );
        h.text("ask John to deploy Kubernetes", 2_100);
        assert_eq!(
            intended(&h.text("ask John to deploy Kubernetes", 3_000)),
            vec!["Kubernetes"]
        );
    }

    #[test]
    fn reverting_an_applied_pair_reaches_the_store() {
        let mut h = Harness::new("ask Marc now");
        h.applied = vec![("mark".to_string(), "marc".to_string())];
        h.text("ask Marc now", 0);
        h.text("ask mark now", 100);
        assert_eq!(intended(&h.text("ask mark now", 1_000)), vec!["mark"]);
    }

    #[test]
    fn pending_dropped_when_the_fix_is_undone() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.text("ask John now", 100);
        h.text("ask Jon now", 200);
        assert!(matches!(
            h.read(FocusRead::FocusChanged, None, 300),
            Decision::Teardown(_)
        ));
    }

    #[test]
    fn no_signal_keeps_pending() {
        let mut h = Harness::new("ask Jon now");
        h.text("ask Jon now", 0);
        h.text("ask John now", 100);
        assert_eq!(h.read(FocusRead::NoSignal, None, 200), Decision::Continue);
        assert_eq!(
            intended(&h.read(FocusRead::NoSignal, None, 1_000)),
            vec!["John"]
        );
    }

    #[test]
    fn toast_groups_ids_by_status() {
        let c = |m: &str, i: &str| Candidate {
            misheard: m.into(),
            intended: i.into(),
            sentence_start: false,
        };
        let entries = vec![
            ("a".to_string(), c("Jon", "John"), CorrectionStatus::Active),
            (
                "b".to_string(),
                c("kas", "k8s"),
                CorrectionStatus::Suggested,
            ),
            (
                "c".to_string(),
                c("Mat", "Matt"),
                CorrectionStatus::Suggested,
            ),
        ];
        let event = toast_event(&entries).expect("event");
        // A suggestion leads: it asks for a decision.
        assert_eq!(event.id, "b");
        assert_eq!(event.status, CorrectionStatus::Suggested);
        assert_eq!(event.suggested_ids, vec!["b", "c"]);
        assert_eq!(event.active_ids, vec!["a"]);
        assert_eq!(event.extra, 2);
        assert!(toast_event(&[]).is_none());
    }

    #[test]
    fn locate_paste_prefers_the_occurrence_at_the_caret() {
        let field = "ok ok";
        assert_eq!(locate_paste(field, "ok", Some(2)), Some(0..2));
        assert_eq!(locate_paste(field, "ok", None), Some(3..5));
        assert_eq!(locate_paste(field, "ok ", Some(5)), Some(3..5));
        assert_eq!(locate_paste(field, "nope", None), None);
    }

    #[test]
    fn remap_span_follows_the_edit() {
        let base = "Hi deploy on Cubernetes now";
        let span = 3..23; // "deploy on Cubernetes"
        let current = "Hi deploy on Kubernetes now";
        assert_eq!(
            &current[remap_span(base, &span, current)],
            "deploy on Kubernetes"
        );
        // Text added before the span shifts it.
        let current = "Hi team, deploy on Cubernetes now";
        assert_eq!(
            &current[remap_span(base, &span, current)],
            "deploy on Cubernetes"
        );
    }

    #[test]
    fn utf16_offsets_convert_to_bytes() {
        let text = "Grüße 😀 x";
        assert_eq!(utf16_to_byte(text, 0), Some(0));
        assert_eq!(utf16_to_byte(text, 3), Some("Grü".len()));
        // The emoji is two UTF-16 units.
        assert_eq!(utf16_to_byte(text, 8), Some("Grüße 😀".len()));
        assert_eq!(utf16_to_byte(text, 10), Some(text.len()));
        assert_eq!(utf16_to_byte(text, 11), None);
    }

    #[test]
    fn related_checks() {
        assert!(is_related_to_snapshot(
            "Ich war in Munchen",
            "Ich war in München"
        ));
        assert!(is_related_to_snapshot(
            "send it to Jon",
            "send it to Jon please"
        ));
        assert!(!is_related_to_snapshot("send it to Jon", ""));
        assert!(!is_related_to_snapshot("yes", "yesterday afternoon"));
        let base = "Hi Cubernetes!";
        assert!(surroundings_survive(base, &(3..13), "Hi Kubernetes!"));
        assert!(!surroundings_survive(base, &(3..13), "Hi !"));
        assert!(!surroundings_survive(base, &(3..13), "Bye Kubernetes!"));
        assert!(!surroundings_survive("Cubernetes", &(0..10), ""));
    }
}
