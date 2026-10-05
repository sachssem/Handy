//! Local dictation journal (fork feature: voice-control).
//!
//! Every dictation is written as structured JSON lines — timings, model and
//! language facts, the text after each pipeline stage, the app context, the
//! post-paste learning outcome and overlay latencies — so an analysis agent
//! can later study and improve quality without asking the user. Schema and
//! example queries: `docs/journal.md`.
//!
//! - One file per local day: `<app log dir>/journal/YYYY-MM-DD.jsonl`.
//! - Retention `dictation_journal_retention_days` (default 90), pruned at
//!   startup and on day rollover; a per-day size cap as a safety valve.
//! - Writes never touch the hot path: records are queued on a bounded channel
//!   and written by one background thread ([`writer`]); a full channel drops
//!   the line. Nothing here can fail or delay a dictation.
//! - Privacy: everything stays on disk locally, nothing is sent anywhere.
//!   Text is written only when the target is verified as a non-secure field
//!   (macOS AX context, secure event input off); otherwise it is redacted —
//!   always on platforms without the Accessibility API.
//!
//! - Disabled = near-zero cost: dictation ids are still issued (ASR biasing
//!   and the output stages key the app context by them), but no fact or text
//!   is copied into a record.
//!
//! Hooks (each marked `fork(voice-control)` in the upstream file):
//! `actions.rs` (begin / start path / mic ready / stop guard), `managers/
//! transcription.rs` (engine facts, allowlist guard, text stages),
//! `transcription_coordinator.rs` ([`AUTO_STOP_TRIGGER`]), `overlay.rs` +
//! `RecordingOverlay.tsx` (overlay breadcrumbs via
//! [`commands::journal_overlay_stage`]), `correction_learning` (learning
//! record).

pub(crate) mod commands;
mod dictation;
pub(crate) mod record;
mod writer;

pub use dictation::{
    begin_dictation, current_id, last_dictation_id, mark_mic_ready, record_allowlist_guard,
    record_allowlist_result, record_asr, record_asr_bias, record_learning, record_model_load,
    record_self_correction, record_start_path, record_text_stage, start_failed, DictationGuard,
    AUTO_STOP_TRIGGER,
};
pub use record::{
    AllowlistResult, AsrFacts, LearningPair, LearningRecord, SelfCorrectionFacts, Stage, StartPath,
};

use record::{Entry, Event};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use writer::JournalWriter;

/// On-disk schema version, written as `v` on every line. Bump on any
/// incompatible field change and note it in `docs/journal.md`.
pub const SCHEMA_VERSION: u32 = 1;

/// Sub-directory of the app log dir holding the day files.
const JOURNAL_DIR: &str = "journal";

static ENABLED: AtomicBool = AtomicBool::new(false);
static WRITER: OnceLock<JournalWriter> = OnceLock::new();
static DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// The journal directory (`<app log dir>/journal`).
pub fn journal_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    if let Some(dir) = DIR.get() {
        return Some(dir.clone());
    }
    crate::portable::app_log_dir(app)
        .ok()
        .map(|dir| dir.join(JOURNAL_DIR))
}

/// Start the writer thread (which prunes old day files first). Called once at
/// startup; the thread runs even while the journal is disabled so toggling it
/// on takes effect immediately.
pub fn init(app: &tauri::AppHandle) {
    let settings = crate::settings::get_settings(app);
    ENABLED.store(settings.dictation_journal_enabled, Ordering::Relaxed);
    let Some(dir) = journal_dir(app) else {
        log::warn!("journal: no app log dir — journal disabled");
        return;
    };
    let _ = DIR.set(dir.clone());
    let _ = WRITER.get_or_init(|| {
        JournalWriter::spawn(
            dir,
            settings.dictation_journal_retention_days,
            writer::MAX_BYTES_PER_DAY,
        )
    });
}

pub(crate) fn set_enabled(enabled: bool) {
    ENABLED.store(enabled, Ordering::Relaxed);
}

pub(crate) fn set_retention_days(days: u32) {
    if let Some(writer) = WRITER.get() {
        writer.set_retention(days);
    }
}

/// Whether the journal is on. Hooks that would copy text or build facts only
/// for the journal check this first.
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Queue one event (no-op while disabled or before [`init`]).
fn emit(event: Event) {
    if !is_enabled() {
        return;
    }
    if let Some(writer) = WRITER.get() {
        writer.send(Entry {
            v: SCHEMA_VERSION,
            ts: now_ms(),
            event,
        });
    }
}

/// Journal a finished app-context capture. Its window title and text before
/// the caret follow the dictation's redaction rule (fails closed).
pub(crate) fn record_context(context: &crate::dictation_context::DictationContext) {
    if !is_enabled() {
        return;
    }
    emit(Event::Context(Box::new(dictation::redacted_context(
        context,
    ))));
}
