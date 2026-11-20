//! fork(voice-control): the dictation journal's engine facts for one
//! transcription (see `crate::journal`). Fork-owned: a child module, so it can
//! read the transcription's run setup without widening upstream's types.

use std::time::Instant;

use super::{RunOutcome, TranscriptionManager};
use crate::settings::AppSettings;

/// Journal the engine run that produced `outcome` (live or batch, after the
/// allowlist guard). Returns before copying anything when the journal is off.
pub(super) fn record(
    tm: &TranscriptionManager,
    settings: &AppSettings,
    outcome: &RunOutcome,
    started: Instant,
    audio_len: usize,
) {
    if !crate::journal::is_enabled() {
        return;
    }
    let loaded = tm.engine.loaded();
    let setup = &outcome.setup;
    let output_language = setup.output_language(settings, outcome.model_detected_language.clone());
    crate::journal::record_asr(crate::journal::AsrFacts {
        model_id: tm
            .get_current_model()
            .unwrap_or_else(|| settings.selected_model.clone()),
        engine: loaded.as_ref().map(|info| info.arch.clone()),
        backend: loaded.map(|info| info.backend),
        accelerator_setting: format!("{:?}", settings.transcribe_accelerator),
        language_setting: settings.selected_language.clone(),
        language_effective: setup.language.intent().to_string(),
        language_hint: setup.applied_language_hint.clone(),
        detected_language: outcome.model_detected_language.clone(),
        language_evidence: format!("{:?}", output_language),
        translated: setup.output_was_translated,
        engine_ms: started.elapsed().as_millis() as u64,
        audio_secs: audio_len as f64 / 16_000.0,
    });
}
