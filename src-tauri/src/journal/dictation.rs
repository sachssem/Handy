//! The in-flight dictation record and the hook functions that fill it.
//!
//! Dictations are serialized by the transcription coordinator (a press during
//! processing is deferred until the pipeline drains), so the newest open
//! record is the "current" one. Pipeline stages that run without an id in
//! scope (the engine run, the text stages) write into it; everything else
//! addresses a record by id so a late call for an old dictation can never leak
//! into a newer one. The coordinator releases a deferred press before the
//! queued main-thread paste of the previous dictation runs, so a record owned
//! by a live [`DictationGuard`] stays open next to the new one until its guard
//! finishes it. All hooks are cheap (a mutex and, for text, one clone) and do
//! nothing when no dictation is active (e.g. the bench harness,
//! `--transcribe-file`) or the journal is disabled.

use super::record::{
    AllowlistAction, AllowlistGuard, AllowlistResult, AsrFacts, DictationRecord, Event,
    LearningRecord, LlmFacts, Outcome, OverlayRecord, PasteFacts, SelfCorrectionFacts, Stage,
    StartPath,
};
use super::{emit, is_enabled, now_ms};
use crate::dictation_context::DictationContext;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

static RECORDS: Mutex<Records> = Mutex::new(Records::new());
/// Id of the most recently begun dictation (0 = none yet).
static LAST_ID: AtomicU64 = AtomicU64::new(0);

/// `stop_trigger` of the recording-limit auto-stop (the transcription
/// coordinator stops with it instead of a shortcut string).
pub const AUTO_STOP_TRIGGER: &str = "auto-stop";

/// Retained audio is 16 kHz mono.
const SAMPLE_RATE: f64 = 16_000.0;

/// Open records beyond this are abandoned oldest-first (a guard that never
/// drops must not grow the list).
const MAX_OPEN: usize = 4;

/// The open dictation records, oldest first; the last one is current.
struct Records {
    open: Vec<Open>,
    /// Original decisions survive finalization until the learning session ends.
    learning_redactions: VecDeque<(u64, Option<&'static str>)>,
}

struct Open {
    record: DictationRecord,
    /// A [`DictationGuard`] owns the record and will finish it.
    guarded: bool,
}

impl Records {
    const fn new() -> Self {
        Self {
            open: Vec::new(),
            learning_redactions: VecDeque::new(),
        }
    }

    fn remember_redaction(&mut self, id: u64, reason: Option<&'static str>) {
        self.learning_redactions.push_back((id, reason));
        // A session lasts at most five minutes; expired decisions fail closed.
        while self.learning_redactions.len() > 64 {
            self.learning_redactions.pop_front();
        }
    }

    fn learning_redaction(&self, id: Option<u64>) -> Option<&'static str> {
        id.and_then(|id| {
            self.learning_redactions
                .iter()
                .rev()
                .find(|(record_id, _)| *record_id == id)
                .map(|(_, reason)| *reason)
        })
        .unwrap_or(Some("unknown_field"))
    }

    fn current(&mut self) -> Option<&mut DictationRecord> {
        self.open.last_mut().map(|o| &mut o.record)
    }

    fn by_id(&mut self, id: u64) -> Option<&mut DictationRecord> {
        self.open
            .iter_mut()
            .find(|o| o.record.id == id)
            .map(|o| &mut o.record)
    }

    /// Open `record` as current. Returns the records it abandons: every
    /// predecessor no guard owns (it never reached a stop), plus the oldest
    /// beyond [`MAX_OPEN`].
    fn begin(&mut self, record: DictationRecord) -> Vec<DictationRecord> {
        let (kept, mut abandoned): (Vec<_>, Vec<_>) = self.open.drain(..).partition(|o| o.guarded);
        self.open = kept;
        self.open.push(Open {
            record,
            guarded: false,
        });
        let excess = self.open.len().saturating_sub(MAX_OPEN);
        abandoned.extend(self.open.drain(..excess));
        abandoned.into_iter().map(|o| o.record).collect()
    }

    /// Hand the newest record no guard owns yet to a guard; returns its id.
    fn guard_newest(&mut self) -> Option<u64> {
        let open = self.open.iter_mut().rev().find(|o| !o.guarded)?;
        open.guarded = true;
        Some(open.record.id)
    }

    fn take(&mut self, id: u64) -> Option<DictationRecord> {
        let idx = self.open.iter().position(|o| o.record.id == id)?;
        Some(self.open.remove(idx).record)
    }
}

fn records() -> MutexGuard<'static, Records> {
    RECORDS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Fill the current record. A no-op while the journal is disabled, so no
/// fact or text is copied for nothing.
fn with_current(f: impl FnOnce(&mut DictationRecord)) {
    if !is_enabled() {
        return;
    }
    if let Some(record) = records().current() {
        f(record);
    }
}

/// Fill record `id` (see [`with_current`]).
fn with_id(id: u64, f: impl FnOnce(&mut DictationRecord)) {
    if !is_enabled() {
        return;
    }
    if let Some(record) = records().by_id(id) {
        f(record);
    }
}

pub fn current_id() -> Option<u64> {
    records().current().map(|r| r.id)
}

/// Next id: the press's epoch ms, bumped to stay strictly increasing.
fn next_id(now: u64) -> u64 {
    let mut prev = LAST_ID.load(Ordering::SeqCst);
    loop {
        let id = now.max(prev + 1);
        match LAST_ID.compare_exchange(prev, id, Ordering::SeqCst, Ordering::SeqCst) {
            Ok(_) => return id,
            Err(actual) => prev = actual,
        }
    }
}

/// Open a new dictation record at the press and return its id. A previous
/// record that never reached a stop is emitted as `abandoned`; one a
/// [`DictationGuard`] owns (its paste may still be queued) is left to it.
pub fn begin_dictation(binding: &str, post_process: bool) -> u64 {
    let now = now_ms();
    let id = next_id(now);
    let record = DictationRecord {
        id,
        binding: binding.to_string(),
        post_process,
        start_ms: now,
        secure_input: crate::secure_input::is_enabled_now(),
        ..Default::default()
    };
    let abandoned = records().begin(record);
    for mut previous in abandoned {
        previous.outcome = Outcome::Abandoned;
        finalize_and_emit(previous, now);
    }
    id
}

/// Id of the most recently begun dictation (the one whose paste a learning
/// session observes).
pub fn last_dictation_id() -> Option<u64> {
    match LAST_ID.load(Ordering::SeqCst) {
        0 => None,
        id => Some(id),
    }
}

pub fn record_start_path(id: u64, start_path: StartPath) {
    with_id(id, |r| r.start_path = Some(start_path));
}

/// The microphone delivered its first samples.
pub fn mark_mic_ready(id: u64) {
    let now = now_ms();
    with_id(id, |r| {
        r.mic_ready_ms = Some(now.saturating_sub(r.start_ms))
    });
}

/// The recording could not start; the record ends here.
pub fn start_failed(id: u64, error: Option<&str>) {
    finish(id, |r| {
        r.error = error.map(str::to_string);
        Outcome::StartFailed
    });
}

/// Counts actually passed per ASR run, including retries (never prompt text).
pub fn record_asr_bias(id: Option<u64>, vocabulary_n: usize, prompt_chars: usize) {
    if let Some(id) = id {
        with_id(id, |r| {
            r.asr_bias.push(super::record::AsrBiasFacts {
                bias_vocab_n: vocabulary_n,
                bias_prompt_chars: prompt_chars,
            });
        });
    }
}

/// A model load (and its warm-up inference) the press kicked off finished.
pub fn record_model_load(load: Duration, warmup_ms: Option<u64>) {
    with_current(|r| {
        r.model_load_ms = Some(load.as_millis() as u64);
        r.model_warmup_ms = warmup_ms;
    });
}

pub fn record_asr(facts: AsrFacts) {
    with_current(|r| r.asr = Some(facts));
}

/// The language-allowlist guard found the primary run out of bounds.
pub fn record_allowlist_guard(
    reason: &str,
    allowlist: &[String],
    fallback_model: Option<&str>,
    primary_text: &str,
) {
    with_current(|r| {
        r.allowlist_guard = Some(AllowlistGuard {
            reason: reason.to_string(),
            allowlist: allowlist.to_vec(),
            action: if fallback_model.is_some() {
                AllowlistAction::FallbackModel
            } else {
                AllowlistAction::PinRetry
            },
            fallback_model: fallback_model.map(str::to_string),
            primary_text: primary_text.to_string(),
            result: None,
            result_text: None,
        })
    });
}

/// How the allowlist guard resolved.
pub fn record_allowlist_result(result: AllowlistResult, text: Option<&str>) {
    with_current(|r| {
        if let Some(guard) = r.allowlist_guard.as_mut() {
            guard.result = Some(result);
            guard.result_text = text.map(str::to_string);
        }
    });
}

pub fn record_text_stage(stage: Stage, text: &str) {
    with_current(|r| r.text.set(stage, text));
}

/// The self-correction pass ran on the current dictation's text.
pub fn record_self_correction(facts: SelfCorrectionFacts) {
    with_current(|r| r.self_correction = Some(facts));
}

pub fn record_learning(mut record: LearningRecord) {
    if !is_enabled() {
        return;
    }
    let reason = records().learning_redaction(record.id);
    redact_learning(&mut record, reason);
    emit(Event::Learning(Box::new(record)));
}

fn redact_learning(record: &mut LearningRecord, reason: Option<&str>) {
    if reason.is_some() {
        for pair in &mut record.committed {
            pair.misheard.clear();
            pair.intended.clear();
        }
    }
}

/// Journal a recording-overlay webview breadcrumb (`overlay: show '<state>'
/// handler|first-frame epoch_ms=N …`, see `RecordingOverlay.tsx`). Other
/// stages are ignored.
pub(super) fn record_overlay_breadcrumb(stage: &str) {
    let Some((state, phase, epoch_ms)) = parse_overlay_breadcrumb(stage) else {
        return;
    };
    let Some(id) = current_id().or_else(last_dictation_id) else {
        return;
    };
    emit(Event::Overlay(OverlayRecord {
        id,
        state,
        phase: phase.to_string(),
        epoch_ms,
        // The id is the press's epoch ms (unless bumped by a few ms).
        since_press_ms: epoch_ms as i64 - id as i64,
    }));
}

fn parse_overlay_breadcrumb(stage: &str) -> Option<(String, &'static str, u64)> {
    let rest = stage.strip_prefix("overlay: show '")?;
    let (state, rest) = rest.split_once("' ")?;
    let phase = if rest.starts_with("handler ") {
        "handler"
    } else if rest.starts_with("first-frame ") {
        "first_frame"
    } else {
        return None;
    };
    let epoch_ms = rest
        .split_whitespace()
        .find_map(|token| token.strip_prefix("epoch_ms="))?
        .parse()
        .ok()?;
    Some((state.to_string(), phase, epoch_ms))
}

/// Take the record `id` (if it is still current), let `outcome` fill in the
/// last facts, and emit it.
fn finish(id: u64, outcome: impl FnOnce(&mut DictationRecord) -> Outcome) {
    let taken = records().take(id);
    if let Some(mut record) = taken {
        record.outcome = outcome(&mut record);
        finalize_and_emit(record, now_ms());
    }
}

fn finalize_and_emit(mut record: DictationRecord, now: u64) {
    record.end_ms = Some(now);
    let context = crate::dictation_context::get(record.id);
    let secure_input = record.secure_input || context.as_ref().is_some_and(|c| c.secure_input);
    let reason = redaction_reason(context.as_deref(), secure_input, cfg!(target_os = "macos"));
    records().remember_redaction(record.id, reason);
    if let Some(reason) = reason {
        record.redact_text(reason);
    }
    emit(Event::Dictation(Box::new(record)));
}

/// The journal copy of an app context: under the dictation's redaction rule
/// its window title and text before the caret are dropped.
pub(super) fn redacted_context(context: &DictationContext) -> DictationContext {
    let mut context = context.clone();
    if let Some(reason) = redaction_reason(
        Some(&context),
        context.secure_input,
        cfg!(target_os = "macos"),
    ) {
        context.window_title = None;
        context.text_before_caret = None;
        context.text_source = None;
        context.text_redacted = Some(reason.to_string());
    }
    context
}

/// Why a record's text must not reach the disk. Fails closed: text is kept
/// only when the app context was captured, its focused element was identified
/// and is not a password field, and secure event input was off. Without the
/// Accessibility API (`can_inspect` false: every platform but macOS) a secure
/// target cannot be ruled out, so text is never journaled there.
fn redaction_reason(
    context: Option<&DictationContext>,
    secure_input: bool,
    can_inspect: bool,
) -> Option<&'static str> {
    if !can_inspect {
        return Some("unverified_platform");
    }
    if secure_input {
        return Some("secure_input");
    }
    match context {
        Some(c) if c.secure => Some("secure_field"),
        Some(c) if c.focused_role.is_some() => None,
        _ => Some("unknown_field"),
    }
}

/// Follows one dictation from the stop request to its end. Created in
/// `TranscribeAction::stop` and moved through the async pipeline (into the
/// paste closure when there is one); its drop finalizes and emits the record
/// with the outcome the recorded facts imply.
pub struct DictationGuard {
    id: Option<u64>,
    /// `PasteMethod` (Debug form) at the stop, journaled with the paste.
    paste_method: String,
    is_cancelled: Box<dyn Fn() -> bool + Send + Sync>,
}

impl DictationGuard {
    /// Take ownership of the newest dictation no guard owns yet; from here
    /// on only this guard (by id) finishes it. `trigger` is the stop's shortcut
    /// string ([`AUTO_STOP_TRIGGER`] for the recording-limit auto-stop).
    pub fn stop(
        trigger: &str,
        paste_method: &crate::settings::PasteMethod,
        is_cancelled: impl Fn() -> bool + Send + Sync + 'static,
    ) -> Self {
        let id = records().guard_newest();
        let now = now_ms();
        if let Some(id) = id.filter(|_| is_enabled()) {
            let secure_input = crate::secure_input::is_enabled_now();
            with_id(id, |r| {
                r.secure_input |= secure_input;
                r.stop_ms = Some(now);
                r.stop_trigger = Some(trigger.to_string());
                r.recording_limit_auto_stop = trigger == AUTO_STOP_TRIGGER;
                r.recorded_wall_secs = Some(now.saturating_sub(r.start_ms) as f64 / 1000.0);
            });
        }
        Self {
            id,
            paste_method: format!("{:?}", paste_method),
            is_cancelled: Box::new(is_cancelled),
        }
    }

    /// The dictation this guard owns, even after a newer dictation begins.
    pub fn id(&self) -> Option<u64> {
        self.id
    }

    fn update(&self, f: impl FnOnce(&mut DictationRecord)) {
        if let Some(id) = self.id {
            with_id(id, f);
        }
    }

    pub fn recorded(&self, samples: usize, wav_file: &str) {
        self.update(|r| {
            r.retained_audio_secs = Some(samples as f64 / SAMPLE_RATE);
            r.wav_file = Some(wav_file.to_string());
        });
    }

    pub fn transcribed(&self, elapsed: Duration, wav_saved: bool) {
        self.update(|r| {
            r.transcription_ms = Some(elapsed.as_millis() as u64);
            r.wav_saved = Some(wav_saved);
        });
    }

    /// Output handling finished: `llm_text` is the LLM result when it
    /// replaced the transcription; `final_text` goes to the paste.
    pub fn processed(
        &self,
        requested: bool,
        llm_text: Option<&str>,
        elapsed: Duration,
        final_text: &str,
    ) {
        self.update(|r| {
            r.llm = requested.then(|| LlmFacts {
                requested,
                applied: llm_text.is_some(),
                ms: elapsed.as_millis() as u64,
            });
            r.text.llm = llm_text.map(str::to_string);
            r.text.final_text = Some(final_text.to_string());
        });
    }

    pub fn pasted(&self, error: Option<&str>, elapsed: Duration) {
        self.update(|r| {
            r.paste = Some(PasteFacts {
                method: self.paste_method.clone(),
                ms: elapsed.as_secs_f64() * 1000.0,
                ok: error.is_none(),
                error: error.map(str::to_string),
            });
        });
    }

    pub fn failed(&self, error: &str) {
        self.update(|r| r.error = Some(error.to_string()));
    }
}

impl Drop for DictationGuard {
    fn drop(&mut self) {
        let Some(id) = self.id else {
            return;
        };
        let cancelled = (self.is_cancelled)();
        finish(id, |r| outcome_of(r, cancelled));
    }
}

fn outcome_of(record: &DictationRecord, cancelled: bool) -> Outcome {
    match &record.paste {
        Some(paste) if paste.ok => Outcome::Pasted,
        Some(_) => Outcome::PasteFailed,
        None if record.error.is_some() => Outcome::Error,
        None if cancelled => Outcome::Cancelled,
        None if record
            .text
            .final_text
            .as_deref()
            .is_some_and(|t| !t.is_empty()) =>
        {
            Outcome::NotPasted
        }
        None => Outcome::Empty,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn learning_pairs_follow_the_original_dictation_redaction() {
        for reason in [
            "secure_input",
            "secure_field",
            "unknown_field",
            "unverified_platform",
        ] {
            let mut records = Records::new();
            records.remember_redaction(7, Some(reason));
            // An unrelated newer dictation must not relax this decision.
            records.remember_redaction(8, None);
            let mut learning = LearningRecord {
                id: Some(7),
                pending_sets: 2,
                committed: vec![super::super::record::LearningPair {
                    misheard: "secret misheard".into(),
                    intended: "secret intended".into(),
                    outcome: "suggested".into(),
                    pair_id: Some("pair-7".into()),
                }],
                ..Default::default()
            };
            let reason = records.learning_redaction(learning.id);
            redact_learning(&mut learning, reason);
            let json = serde_json::to_string(&learning).unwrap();
            assert!(!json.contains("secret"), "{reason:?}: {json}");
            assert_eq!(learning.committed.len(), 1);
            assert_eq!(learning.committed[0].outcome, "suggested");
            assert_eq!(learning.committed[0].pair_id.as_deref(), Some("pair-7"));
            assert_eq!(learning.pending_sets, 2);
            assert_eq!(records.learning_redaction(Some(8)), None);
            assert!(records.learning_redaction(None).is_some());
            assert!(records.learning_redaction(Some(99)).is_some());
        }
    }

    #[test]
    fn verified_non_secure_learning_keeps_pair_text() {
        let mut records = Records::new();
        records.remember_redaction(7, None);
        let mut learning = LearningRecord {
            id: Some(7),
            committed: vec![super::super::record::LearningPair {
                misheard: "Jon".into(),
                intended: "John".into(),
                outcome: "suggested".into(),
                pair_id: None,
            }],
            ..Default::default()
        };
        let reason = records.learning_redaction(learning.id);
        redact_learning(&mut learning, reason);
        assert_eq!(learning.committed[0].misheard, "Jon");
        assert_eq!(learning.committed[0].intended, "John");
    }

    #[test]
    fn learning_redaction_retention_is_bounded_and_expired_ids_fail_closed() {
        let mut records = Records::new();
        for id in 0..100 {
            records.remember_redaction(id, None);
        }
        assert_eq!(records.learning_redactions.len(), 64);
        assert_eq!(records.learning_redaction(Some(0)), Some("unknown_field"));
        assert_eq!(records.learning_redaction(Some(99)), None);
    }

    #[test]
    fn overlay_breadcrumbs_parse_handler_and_first_frame() {
        assert_eq!(
            parse_overlay_breadcrumb(
                "overlay: show 'recording' handler epoch_ms=1759480000123 render=1.4ms"
            ),
            Some(("recording".to_string(), "handler", 1_759_480_000_123))
        );
        assert_eq!(
            parse_overlay_breadcrumb(
                "overlay: show 'transcribing' first-frame epoch_ms=1759480000150 +12.3ms after handler"
            ),
            Some(("transcribing".to_string(), "first_frame", 1_759_480_000_150))
        );
        assert_eq!(parse_overlay_breadcrumb("toast mounted"), None);
        assert_eq!(
            parse_overlay_breadcrumb("overlay: show 'recording' handler render=1ms"),
            None
        );
    }

    #[test]
    fn ids_are_strictly_increasing() {
        let a = next_id(1_000);
        let b = next_id(1_000);
        let c = next_id(1);
        assert!(b > a && c > b);
    }

    #[test]
    fn outcome_follows_the_recorded_facts() {
        let mut r = DictationRecord::default();
        assert_eq!(outcome_of(&r, false), Outcome::Empty);
        assert_eq!(outcome_of(&r, true), Outcome::Cancelled);
        r.text.final_text = Some("Hallo".into());
        assert_eq!(outcome_of(&r, false), Outcome::NotPasted);
        r.error = Some("boom".into());
        assert_eq!(outcome_of(&r, true), Outcome::Error);
        r.paste = Some(PasteFacts {
            ok: true,
            ..Default::default()
        });
        r.self_correction = Some(SelfCorrectionFacts {
            candidate: Some("geheim".into()),
            reason: "unchanged".into(),
            ..Default::default()
        });
        assert_eq!(outcome_of(&r, false), Outcome::Pasted);
        r.paste = Some(PasteFacts::default());
        assert_eq!(outcome_of(&r, false), Outcome::PasteFailed);
    }

    #[test]
    fn redaction_drops_every_text() {
        let mut r = DictationRecord::default();
        r.text.set(Stage::Asr, "geheim");
        r.text.final_text = Some("geheim".into());
        r.allowlist_guard = Some(AllowlistGuard {
            primary_text: "geheim".into(),
            result_text: Some("geheim".into()),
            ..Default::default()
        });
        r.redact_text("secure_field");
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("geheim"), "{json}");
        assert!(!json.contains("candidate"), "{json}");
        assert!(json.contains("\"text_redacted\":\"secure_field\""));
    }

    fn record(id: u64) -> DictationRecord {
        DictationRecord {
            id,
            ..Default::default()
        }
    }

    fn ids(records: &[DictationRecord]) -> Vec<u64> {
        records.iter().map(|r| r.id).collect()
    }

    #[test]
    fn a_press_never_abandons_a_record_a_guard_owns() {
        let mut records = Records::new();
        assert!(records.begin(record(1)).is_empty());
        // Stop: the guard owns dictation 1; its paste is still queued.
        let mut guard = DictationGuard {
            id: records.guard_newest(),
            paste_method: String::new(),
            is_cancelled: Box::new(|| false),
        };
        assert_eq!(guard.id(), Some(1));
        // A deferred press begins dictation 2 before that paste runs.
        assert!(records.begin(record(2)).is_empty());
        assert_eq!(records.current().map(|r| r.id), Some(2));
        assert_eq!(guard.id(), Some(1), "the queued paste keeps its owner");
        // The guard still reaches and finishes its own record by id.
        assert!(records.by_id(1).is_some());
        assert_eq!(records.take(1).map(|r| r.id), Some(1));
        assert_eq!(records.current().map(|r| r.id), Some(2));
        // A press without a stop still abandons the unguarded predecessor.
        assert_eq!(ids(&records.begin(record(3))), [2]);
        // This test owns local records; do not finalize a global record on drop.
        guard.id = None;
        assert_eq!(guard.id(), None);
    }

    #[test]
    fn a_stop_takes_the_newest_unowned_record_and_open_records_are_bounded() {
        let mut records = Records::new();
        records.begin(record(1));
        assert_eq!(records.guard_newest(), Some(1));
        assert_eq!(records.guard_newest(), None, "no second owner");
        for id in 2..=MAX_OPEN as u64 {
            records.begin(record(id));
            assert_eq!(records.guard_newest(), Some(id));
        }
        // One more press past the bound drops the oldest owned record.
        assert_eq!(ids(&records.begin(record(99))), [1]);
        assert_eq!(records.open.len(), MAX_OPEN);
        assert!(records.take(1).is_none());
    }

    #[test]
    fn redaction_fails_closed_without_a_verified_non_secure_field() {
        let field = DictationContext {
            focused_role: Some("AXTextArea".into()),
            ..Default::default()
        };
        assert_eq!(redaction_reason(Some(&field), false, true), None);
        let password = DictationContext {
            secure: true,
            ..field.clone()
        };
        assert_eq!(
            redaction_reason(Some(&password), false, true),
            Some("secure_field")
        );
        assert_eq!(
            redaction_reason(Some(&field), true, true),
            Some("secure_input")
        );
        // Capture pending / failed, or no focused element identified.
        assert_eq!(redaction_reason(None, false, true), Some("unknown_field"));
        assert_eq!(
            redaction_reason(Some(&DictationContext::default()), false, true),
            Some("unknown_field")
        );
        // No Accessibility API: never journal text.
        assert_eq!(
            redaction_reason(Some(&field), false, false),
            Some("unverified_platform")
        );
    }

    #[test]
    fn the_journal_context_copy_follows_the_redaction_rule() {
        let field = DictationContext {
            focused_role: Some("AXTextArea".into()),
            window_title: Some("Inbox".into()),
            text_before_caret: Some("Hallo".into()),
            text_source: Some("value".into()),
            ..Default::default()
        };
        let can_inspect = cfg!(target_os = "macos");
        let kept = redacted_context(&field);
        assert_eq!(kept.text_before_caret.is_some(), can_inspect);
        assert_eq!(kept.window_title.is_some(), can_inspect);
        let unknown = redacted_context(&DictationContext {
            focused_role: None,
            ..field.clone()
        });
        assert!(unknown.window_title.is_none() && unknown.text_before_caret.is_none());
        assert!(unknown.text_source.is_none() && unknown.text_redacted.is_some());
        let secure_input = redacted_context(&DictationContext {
            secure_input: true,
            ..field
        });
        assert!(secure_input.text_before_caret.is_none());
        if can_inspect {
            assert_eq!(secure_input.text_redacted.as_deref(), Some("secure_input"));
        }
    }

    #[test]
    fn allowlist_guard_serializes_snake_case_strings() {
        let guard = AllowlistGuard {
            action: AllowlistAction::FallbackModel,
            result: Some(AllowlistResult::PinRetryOk),
            ..Default::default()
        };
        let value = serde_json::to_value(&guard).unwrap();
        assert_eq!(value["action"], "fallback_model");
        assert_eq!(value["result"], "pin_retry_ok");
        assert_eq!(
            serde_json::to_value(AllowlistAction::default()).unwrap(),
            "pin_retry"
        );
    }

    #[test]
    fn dictation_record_serializes_with_envelope() {
        let entry = super::super::record::Entry {
            v: super::super::SCHEMA_VERSION,
            ts: 5,
            event: Event::Dictation(Box::new(DictationRecord {
                id: 3,
                outcome: Outcome::Pasted,
                ..Default::default()
            })),
        };
        let value: serde_json::Value = serde_json::to_value(&entry).unwrap();
        assert_eq!(value["type"], "dictation");
        assert_eq!(value["v"], super::super::SCHEMA_VERSION);
        assert_eq!(value["id"], 3);
        assert_eq!(value["outcome"], "pasted");
        assert!(value["text"].get("final").is_some());
    }
}
