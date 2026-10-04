//! Journal record types — the on-disk schema (see `docs/journal.md`).
//!
//! Every line is one [`Entry`]: the envelope (`v`, `ts`, `type`) flattened
//! together with one event body. Field names are the contract an analysis
//! agent reads, so rename nothing without bumping [`super::SCHEMA_VERSION`].

use serde::Serialize;
use std::collections::BTreeMap;

/// One journal line.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Schema version ([`super::SCHEMA_VERSION`]).
    pub v: u32,
    /// Epoch ms at which the line was emitted.
    pub ts: u64,
    #[serde(flatten)]
    pub event: Event,
}

/// The event body; `type` names the variant.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    Dictation(Box<DictationRecord>),
    Context(Box<crate::dictation_context::DictationContext>),
    Learning(Box<LearningRecord>),
    Overlay(OverlayRecord),
}

/// How a dictation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Final text was pasted successfully.
    Pasted,
    /// The paste itself failed.
    PasteFailed,
    /// The pipeline produced text but it never reached the paste call (the
    /// main-thread dispatch failed).
    NotPasted,
    /// No audio, or the pipeline produced empty text.
    Empty,
    /// Cancelled after the stop (during transcription / output handling).
    Cancelled,
    /// Transcription failed.
    Error,
    /// The microphone could not be opened.
    StartFailed,
    /// Never stopped: cancelled while recording, or superseded. Emitted when
    /// the next dictation begins.
    #[default]
    Abandoned,
}

/// Pre-recording steps of `TranscribeAction::start`, in ms. Each one is added
/// press→capture latency.
#[derive(Debug, Clone, Default, Serialize)]
pub struct StartPath {
    pub model_kickoff_ms: f64,
    pub stream_plan_ms: f64,
    pub overlay_ms: f64,
    pub tray_ms: f64,
    /// Model selected at press time (the engine run may differ, see `asr`).
    pub selected_model: String,
    pub streaming: bool,
    pub vad: String,
}

/// Facts about the engine run that produced the text.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AsrFacts {
    pub model_id: String,
    /// `EngineType` of the model (e.g. `TranscribeCpp`, `Parakeet`).
    pub engine: Option<String>,
    /// Compute backend actually bound (transcribe-cpp backend string, `onnx`).
    pub backend: Option<String>,
    /// Persisted accelerator setting (transcribe-cpp).
    pub accelerator_setting: String,
    /// Persisted language intent (`auto`, `de`, …).
    pub language_setting: String,
    /// Language after model-capability coercion.
    pub language_effective: String,
    /// Language hint actually passed to the engine run (`None` = auto).
    pub language_hint: Option<String>,
    /// Language the model itself detected (audio LID), if it reports one.
    pub detected_language: Option<String>,
    /// Final `OutputLanguageEvidence` (Debug form) used by later stages.
    pub language_evidence: String,
    pub translated: bool,
    /// Engine run wall time (incl. any allowlist retry / fallback), ms.
    pub engine_ms: u64,
    /// Seconds of audio handed to the engine.
    pub audio_secs: f64,
}

/// What the language-allowlist guard did about an out-of-bounds run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistAction {
    /// Re-ran the active model pinned to the first allowlisted language.
    #[default]
    PinRetry,
    /// Re-transcribed with the configured fallback model.
    FallbackModel,
}

/// How the language-allowlist guard resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AllowlistResult {
    FallbackAccepted,
    FallbackRejected,
    PinRetryOk,
    PinRetryFailed,
    PinUnsupported,
}

/// The language-allowlist guard fired on this dictation.
#[derive(Debug, Clone, Default, Serialize)]
pub struct AllowlistGuard {
    /// Why it fired: `detected language 'ru'` / `dominant Cyrillic script`.
    pub reason: String,
    pub allowlist: Vec<String>,
    pub action: AllowlistAction,
    pub fallback_model: Option<String>,
    /// The out-of-bounds primary transcript.
    pub primary_text: String,
    pub result: Option<AllowlistResult>,
    /// Text the guard produced (accepted fallback / pinned retry).
    pub result_text: Option<String>,
}

/// The text after each pipeline stage. `None` = the stage did not run.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TextStages {
    /// Raw engine output (after allowlist handling).
    pub asr: Option<String>,
    /// After fuzzy custom-word correction (`None` when not applied, e.g. the
    /// words were passed as a whisper prompt instead).
    pub custom_words: Option<String>,
    /// After filler-word removal.
    pub fillers: Option<String>,
    /// After whitespace/punctuation normalization.
    pub normalized: Option<String>,
    /// After the fork's text rules.
    pub text_rules: Option<String>,
    /// After learned corrections — the transcription result.
    pub learned: Option<String>,
    /// LLM post-processing output, when it ran and returned text.
    pub llm: Option<String>,
    /// After snippet expansion (`None` = no snippet
    /// fired); recorded between `normalized` and `text_rules`.
    pub snippets: Option<String>,
    /// Accepted self-correction LLM result (`None` = not applied).
    pub self_correction: Option<String>,
    /// After the per-app style stage (`None` = stage off / no change).
    pub app_style: Option<String>,
    /// The text handed to the paste.
    #[serde(rename = "final")]
    pub final_text: Option<String>,
}

/// Pipeline stage identifiers for [`TextStages`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Asr,
    CustomWords,
    Fillers,
    Normalized,
    TextRules,
    Learned,
    Snippets,
    SelfCorrection,
    AppStyle,
}

impl TextStages {
    /// Record `stage`'s text. A new `Asr` text starts a fresh run (a stream
    /// finalize that fell back to batch), so later stages never mix runs.
    pub fn set(&mut self, stage: Stage, text: &str) {
        if stage == Stage::Asr {
            self.clear();
        }
        let slot = match stage {
            Stage::Asr => &mut self.asr,
            Stage::CustomWords => &mut self.custom_words,
            Stage::Fillers => &mut self.fillers,
            Stage::Normalized => &mut self.normalized,
            Stage::TextRules => &mut self.text_rules,
            Stage::Learned => &mut self.learned,
            Stage::Snippets => &mut self.snippets,
            Stage::SelfCorrection => &mut self.self_correction,
            Stage::AppStyle => &mut self.app_style,
        };
        *slot = Some(text.to_string());
    }

    fn clear(&mut self) {
        *self = TextStages::default();
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmFacts {
    /// The post-process binding was used.
    pub requested: bool,
    /// The LLM returned text that replaced the transcription.
    pub applied: bool,
    /// Output handling wall time (LLM call included), ms.
    pub ms: u64,
}

/// The trigger-gated self-correction LLM pass (see `self_correction`).
#[derive(Debug, Clone, Default, Serialize)]
pub struct SelfCorrectionFacts {
    /// The pass was enabled when the cue was found.
    pub requested: bool,
    /// The LLM result replaced the text.
    pub applied: bool,
    /// Wall time of the pass (LLM call included), ms.
    pub ms: u64,
    /// `applied` or why not: `disabled`, `snippet`, `post_process_applied`,
    /// `no_provider`, `apple_unavailable`, `timeout`, `error`, `empty`,
    /// `unchanged`, `length_ratio`, `added_words`.
    pub reason: String,
    /// The detected cue, e.g. `nein warte`.
    pub cue: String,
    /// `rules`, `apple_intelligence` or the post-process provider id.
    pub provider: Option<String>,
    /// Cleaned LLM output when rejected (including unchanged). Local journal
    /// only; omitted when text is redacted or when no candidate was rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PasteFacts {
    /// `PasteMethod` (Debug form).
    pub method: String,
    pub ms: f64,
    pub ok: bool,
    pub error: Option<String>,
}

/// Privacy-safe counts for one engine run, in run order (primary / retry).
#[derive(Debug, Clone, Default, Serialize)]
pub struct AsrBiasFacts {
    pub bias_vocab_n: usize,
    pub bias_prompt_chars: usize,
}

/// One dictation, press to paste.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DictationRecord {
    /// Dictation id: epoch ms of the press, strictly increasing. Joins
    /// `context`, `learning` and `overlay` events.
    pub id: u64,
    pub binding: String,
    pub post_process: bool,
    /// Press (`TranscribeAction::start`) epoch ms.
    pub start_ms: u64,
    /// Stop request epoch ms.
    pub stop_ms: Option<u64>,
    /// Epoch ms when the record was finalized.
    pub end_ms: Option<u64>,
    /// Shortcut string of the stop, or `auto-stop`.
    pub stop_trigger: Option<String>,
    /// The recording-limit auto-stop ended the recording.
    pub recording_limit_auto_stop: bool,
    pub start_path: Option<StartPath>,
    /// Press → first microphone samples, ms.
    pub mic_ready_ms: Option<u64>,
    /// Press → stop, seconds.
    pub recorded_wall_secs: Option<f64>,
    /// Audio retained after VAD trimming, seconds.
    pub retained_audio_secs: Option<f64>,
    pub wav_file: Option<String>,
    pub wav_saved: Option<bool>,
    pub asr: Option<AsrFacts>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub asr_bias: Vec<AsrBiasFacts>,
    pub allowlist_guard: Option<AllowlistGuard>,
    /// Stop pipeline: stream finalize / batch transcribe call, ms.
    pub transcription_ms: Option<u64>,
    pub text: TextStages,
    /// Why the text was dropped: `secure_field`, `secure_input`,
    /// `unknown_field` or `unverified_platform` (see `docs/journal.md`).
    pub text_redacted: Option<String>,
    /// Secure event input was on at the press or the stop (not serialized).
    #[serde(skip)]
    pub secure_input: bool,
    pub llm: Option<LlmFacts>,
    /// The self-correction LLM pass, when a cue was found.
    pub self_correction: Option<SelfCorrectionFacts>,
    pub paste: Option<PasteFacts>,
    pub outcome: Outcome,
    pub error: Option<String>,
}

impl DictationRecord {
    /// Drop every captured text (secure-field target).
    pub fn redact_text(&mut self, reason: &str) {
        self.text.clear();
        if let Some(facts) = self.self_correction.as_mut() {
            facts.candidate = None;
        }
        if let Some(guard) = self.allowlist_guard.as_mut() {
            guard.primary_text.clear();
            guard.result_text = None;
        }
        self.text_redacted = Some(reason.to_string());
    }
}

/// One committed / observed pair of a learning session.
#[derive(Debug, Clone, Serialize)]
pub struct LearningPair {
    pub misheard: String,
    pub intended: String,
    /// Store outcome: `suggested`, `promoted`, `re-observed`,
    /// `reverted+blocked`, `blocked`, `manual kept`, `inverse of manual`,
    /// `store full`.
    pub outcome: String,
    pub pair_id: Option<String>,
}

/// Post-paste correction-learning session outcome.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LearningRecord {
    /// Dictation whose paste opened the session.
    pub id: Option<u64>,
    /// Session generation (matches the `learn: session N` debug lines).
    pub session: u64,
    pub app: Option<String>,
    pub lang: Option<String>,
    pub window_secs: u64,
    pub observer: bool,
    /// Set when no session ran (`no frontmost pid`, `snapshot failed`, …).
    pub skipped: Option<String>,
    /// `exact` (paste found in field) | `paste_only` (fallback anchor).
    pub anchored: Option<String>,
    /// Field reads performed.
    pub reads: u32,
    /// Reads whose text differed from the previous read.
    pub edits_observed: u32,
    pub unrelated: u32,
    /// Candidate rejections per gate name.
    pub gate_rejections: BTreeMap<String, u32>,
    pub reformulations: u32,
    pub oversized: u32,
    /// Times a (new) candidate set became pending.
    pub pending_sets: u32,
    pub pending_dropped: u32,
    /// Why the session ended (`settled`, `window elapsed`, `focus moved`, …).
    pub end_reason: Option<String>,
    pub committed: Vec<LearningPair>,
    pub duration_ms: u64,
}

/// A recording-overlay webview breadcrumb.
#[derive(Debug, Clone, Serialize)]
pub struct OverlayRecord {
    pub id: u64,
    /// Overlay state shown (`recording`, `transcribing`, …).
    pub state: String,
    /// `handler` (event handler entry) | `first_frame`.
    pub phase: String,
    pub epoch_ms: u64,
    /// `epoch_ms` minus the dictation's press.
    pub since_press_ms: i64,
}
