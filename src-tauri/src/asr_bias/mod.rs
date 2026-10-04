//! Capability-gated ASR vocabulary and app context (fork feature: voice-control).

use crate::correction_learning::LearnedCorrection;
use crate::dictation_context::DictationContext;
use crate::settings::{self, AppSettings};
use std::collections::HashSet;
use std::sync::Mutex;
use transcribe_cpp::{Feature, Model, RunExtension, RunOptions};

const MAX_TERMS: usize = 40;
const MAX_TERM_CHARS: usize = 40;
const MAX_TEXT_CHARS: usize = 300;

#[derive(Debug, Default)]
pub struct AsrBias {
    pub vocabulary: Vec<String>,
    pub prompt: Option<String>,
}

/// Take only context already captured for this dictation; never wait for AX.
pub fn build(settings: &AppSettings, dictation_id: Option<u64>) -> AsrBias {
    if !settings.asr_context_biasing_enabled {
        return AsrBias::default();
    }
    let corrections = crate::correction_learning::snapshot();
    let context = dictation_id.and_then(crate::dictation_context::get);
    AsrBias {
        vocabulary: vocabulary(&settings.custom_words, &corrections.corrections),
        prompt: context.as_deref().and_then(context_prompt),
    }
}

/// Apply only fields supported by this model and record counts for every run.
///
/// Returns the options as they were before biasing when bias was applied, so
/// the caller can retry once without it ([`run`]). Records whether `model_id`
/// accepts any bias for the settings UI ([`get_asr_bias_support`]).
pub fn apply(
    settings: &AppSettings,
    dictation_id: Option<u64>,
    model_id: &str,
    model: &Model,
    options: &mut RunOptions,
) -> Option<RunOptions> {
    let vocabulary_ok = model.supports(Feature::Vocabulary);
    let prompt_ok = model.supports(Feature::ContextPrompt);
    record_support(model_id, vocabulary_ok || prompt_ok);
    let bias = if vocabulary_ok || prompt_ok {
        build(settings, dictation_id)
    } else {
        AsrBias::default()
    };
    let unbiased = options.clone();
    let applied = merge(bias, vocabulary_ok, prompt_ok, options);
    let prompt_chars = options.prompt.as_deref().map_or(0, |p| p.chars().count());
    log::debug!(
        "asr-bias: vocab={} prompt_chars={}",
        options.vocabulary.len(),
        prompt_chars
    );
    crate::journal::record_asr_bias(dictation_id, options.vocabulary.len(), prompt_chars);
    applied.then_some(unbiased)
}

/// Fold `bias` into `options`; returns whether anything was applied.
///
/// Whisper's generic vocabulary/prompt and its extension `initial_prompt` fill
/// the same decoder slot, and transcribe-cpp rejects a run carrying both
/// (INVALID_ARG). Only one survives: the generic bias when it carries
/// vocabulary (custom words lead it, so the initial prompt's terms are kept),
/// otherwise the initial prompt and no generic bias at all.
fn merge(
    mut bias: AsrBias,
    vocabulary_ok: bool,
    prompt_ok: bool,
    options: &mut RunOptions,
) -> bool {
    if !vocabulary_ok {
        bias.vocabulary.clear();
    }
    if !prompt_ok {
        bias.prompt = None;
    }
    if bias.vocabulary.is_empty() && bias.prompt.is_none() {
        return false;
    }
    if let Some(RunExtension::Whisper(whisper)) = options.family.as_mut() {
        if whisper
            .initial_prompt
            .as_deref()
            .is_some_and(|p| !p.is_empty())
        {
            if bias.vocabulary.is_empty() {
                return false;
            }
            whisper.initial_prompt = None;
        }
    }
    options.vocabulary = bias.vocabulary;
    options.prompt = bias.prompt;
    true
}

/// One engine run for the current dictation: bias `options` for `model_id`
/// ([`apply`]), run, and retry once unbiased if the engine rejects the bias
/// ([`run`]).
pub fn run_biased<T>(
    settings: &AppSettings,
    model_id: &str,
    model: &Model,
    mut options: RunOptions,
    run_engine: impl FnMut(&RunOptions) -> transcribe_cpp::Result<T>,
) -> transcribe_cpp::Result<T> {
    let dictation_id = crate::journal::current_id();
    let unbiased = apply(settings, dictation_id, model_id, model, &mut options);
    run(&options, unbiased.as_ref(), dictation_id, run_engine)
}

/// Run `run` with the biased `options`; when the engine rejects them as an
/// invalid argument (a bias/extension clash, a NUL or token literal that
/// slipped through) and `unbiased` is set, retry once without the bias.
fn run<T>(
    options: &RunOptions,
    unbiased: Option<&RunOptions>,
    dictation_id: Option<u64>,
    mut run: impl FnMut(&RunOptions) -> transcribe_cpp::Result<T>,
) -> transcribe_cpp::Result<T> {
    match (run(options), unbiased) {
        (Err(err), Some(unbiased)) if is_bias_rejection(&err) => {
            log::warn!("asr-bias: biased run rejected ({err}); retrying without bias");
            crate::journal::record_asr_bias(dictation_id, 0, 0);
            run(unbiased)
        }
        (result, _) => result,
    }
}

fn is_bias_rejection(err: &transcribe_cpp::Error) -> bool {
    matches!(
        err,
        transcribe_cpp::Error::InvalidArgument(_) | transcribe_cpp::Error::Nul(_)
    )
}

/// Last observed bias support per model id (filled on every run).
static SUPPORT: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

fn record_support(model_id: &str, supported: bool) {
    let mut support = SUPPORT.lock().unwrap_or_else(|e| e.into_inner());
    match support.iter_mut().find(|(id, _)| id == model_id) {
        Some(entry) => entry.1 = supported,
        None => support.push((model_id.to_string(), supported)),
    }
}

/// Text the engine may not see: control characters (NUL included) become
/// spaces and `<|…|>` special-token literals are dropped.
fn sanitize(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find("<|") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        rest = match after.find("|>") {
            Some(end) => &after[end + 2..],
            None => after,
        };
    }
    out.push_str(rest);
    out
}

fn vocabulary(custom_words: &[String], corrections: &[LearnedCorrection]) -> Vec<String> {
    let mut active: Vec<_> = corrections
        .iter()
        .filter(|pair| pair.is_applied())
        .collect();
    active.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| b.last_seen.cmp(&a.last_seen))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = HashSet::new();
    custom_words
        .iter()
        .map(String::as_str)
        .chain(active.iter().map(|pair| pair.intended.as_str()))
        .map(|term| sanitize(term).trim().to_string())
        .filter(|term| !term.is_empty() && term.chars().count() <= MAX_TERM_CHARS)
        .filter(|term| seen.insert(term.to_lowercase()))
        .take(MAX_TERMS)
        .collect()
}

fn compact(text: &str, max_chars: usize) -> String {
    sanitize(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(max_chars)
        .collect()
}

fn context_prompt(context: &DictationContext) -> Option<String> {
    let mut parts = Vec::new();
    for (label, value, cap) in [
        ("App", context.app_name.as_deref(), 120),
        ("Window", context.window_title.as_deref(), 180),
    ] {
        if let Some(value) = value {
            let value = compact(value, cap);
            if !value.is_empty() {
                parts.push(format!("{label}: {value}."));
            }
        }
    }
    // Defense in depth: capture already refuses to read secure-field text.
    if !context.secure {
        if let Some(text) = context.text_before_caret.as_deref() {
            let skip = text.chars().count().saturating_sub(MAX_TEXT_CHARS);
            let tail: String = text.chars().skip(skip).collect();
            let tail = compact(&tail, MAX_TEXT_CHARS);
            if !tail.is_empty() {
                parts.push(format!("Text before cursor: {tail}"));
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join(" "))
}

#[tauri::command]
#[specta::specta]
pub fn change_asr_context_biasing_setting(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.asr_context_biasing_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

/// Whether `model_id` accepted vocabulary or app context on its last run this
/// session; `None` until it has run.
#[tauri::command]
#[specta::specta]
pub fn get_asr_bias_support(model_id: String) -> Option<bool> {
    SUPPORT
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .find(|(id, _)| *id == model_id)
        .map(|(_, supported)| *supported)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_learning::CorrectionStatus;

    fn pair(intended: &str, count: u32, last_seen: i64) -> LearnedCorrection {
        let mut pair: LearnedCorrection = serde_json::from_value(serde_json::json!({
            "id": intended, "misheard": "wrong", "intended": intended,
            "count": count, "last_seen": last_seen, "source": "manual", "enabled": true
        }))
        .expect("valid correction fixture");
        pair.status = CorrectionStatus::Active;
        pair
    }

    #[test]
    fn custom_words_first_active_pairs_ranked_and_case_insensitively_deduplicated() {
        let mut disabled = pair("disabled", 99, 99);
        disabled.enabled = false;
        let mut suggested = pair("suggested", 99, 99);
        suggested.status = CorrectionStatus::Suggested;
        let pairs = vec![
            pair("older", 3, 1),
            pair("recent", 3, 2),
            pair("common", 4, 0),
            pair("RUST", 100, 100),
            disabled,
            suggested,
        ];
        assert_eq!(
            vocabulary(&[" Rust ".into(), "rust".into(), " Qwen ".into()], &pairs),
            ["Rust", "Qwen", "common", "recent", "older"]
        );
    }

    #[test]
    fn term_and_dictionary_limits_count_unicode_chars() {
        let forty = "é".repeat(40);
        let mut words = vec!["  ".into(), "é".repeat(41), forty.clone()];
        words.extend((0..50).map(|n| format!("word{n}")));
        let terms = vocabulary(&words, &[pair("learned", 100, 100)]);
        assert_eq!(terms.len(), 40);
        assert_eq!(terms[0], forty);
        assert_eq!(terms[39], "word38");
        assert!(!terms.iter().any(|s| s == "learned"));
    }

    #[test]
    fn context_is_compact_and_uses_the_last_300_chars() {
        let context = DictationContext {
            app_name: Some("  Editor\n app ".into()),
            window_title: Some("  Notes ".into()),
            text_before_caret: Some(format!("prefix{}", "é".repeat(300))),
            ..Default::default()
        };
        assert_eq!(
            context_prompt(&context),
            Some(format!(
                "App: Editor app. Window: Notes. Text before cursor: {}",
                "é".repeat(300)
            ))
        );
    }

    #[test]
    fn empty_context_is_none_and_secure_field_text_is_excluded() {
        assert!(context_prompt(&DictationContext::default()).is_none());
        let context = DictationContext {
            secure: true,
            text_before_caret: Some("secret".into()),
            app_name: Some("Editor".into()),
            ..Default::default()
        };
        assert_eq!(context_prompt(&context).as_deref(), Some("App: Editor."));
    }

    #[test]
    fn missing_persisted_setting_defaults_on() {
        let settings = settings::get_default_settings();
        assert!(settings.asr_context_biasing_enabled);
        let mut persisted = serde_json::to_value(&settings).expect("serialize settings");
        persisted
            .as_object_mut()
            .expect("settings object")
            .remove("asr_context_biasing_enabled");
        let restored: AppSettings =
            serde_json::from_value(persisted).expect("restore older settings");
        assert!(restored.asr_context_biasing_enabled);
    }

    #[test]
    fn disabled_biasing_and_missing_context_do_not_use_previous_context() {
        let mut settings = settings::get_default_settings();
        settings.custom_words = vec!["Rust".into()];
        settings.asr_context_biasing_enabled = false;
        let bias = build(&settings, None);
        assert!(bias.vocabulary.is_empty() && bias.prompt.is_none());
        settings.asr_context_biasing_enabled = true;
        let bias = build(&settings, None);
        assert_eq!(bias.vocabulary.first().map(String::as_str), Some("Rust"));
        assert!(bias.prompt.is_none());
    }

    fn whisper(initial_prompt: Option<&str>) -> RunOptions {
        RunOptions {
            family: Some(RunExtension::Whisper(transcribe_cpp::WhisperRunOptions {
                initial_prompt: initial_prompt.map(str::to_string),
                ..Default::default()
            })),
            ..Default::default()
        }
    }

    fn initial_prompt(options: &RunOptions) -> Option<&str> {
        match options.family.as_ref() {
            Some(RunExtension::Whisper(w)) => w.initial_prompt.as_deref(),
            _ => None,
        }
    }

    fn bias(vocabulary: &[&str], prompt: Option<&str>) -> AsrBias {
        AsrBias {
            vocabulary: vocabulary.iter().map(|s| s.to_string()).collect(),
            prompt: prompt.map(str::to_string),
        }
    }

    #[test]
    fn generic_vocabulary_replaces_the_whisper_initial_prompt() {
        let mut options = whisper(Some("Rust, Qwen"));
        assert!(merge(
            bias(&["Rust", "Qwen"], Some("App: X.")),
            true,
            true,
            &mut options
        ));
        assert_eq!(initial_prompt(&options), None);
        assert_eq!(options.vocabulary, ["Rust", "Qwen"]);
        assert_eq!(options.prompt.as_deref(), Some("App: X."));
    }

    #[test]
    fn prompt_only_bias_never_displaces_the_initial_prompt() {
        // No vocabulary support: generic bias would drop the custom words.
        let mut options = whisper(Some("Rust"));
        assert!(!merge(
            bias(&["Rust"], Some("App: X.")),
            false,
            true,
            &mut options
        ));
        assert_eq!(initial_prompt(&options), Some("Rust"));
        assert!(options.vocabulary.is_empty() && options.prompt.is_none());
    }

    #[test]
    fn unsupported_or_empty_bias_leaves_options_untouched() {
        let mut options = whisper(Some("Rust"));
        assert!(!merge(
            bias(&["Rust"], Some("p")),
            false,
            false,
            &mut options
        ));
        assert!(!merge(AsrBias::default(), true, true, &mut options));
        assert_eq!(options, whisper(Some("Rust")));
        // Without an initial prompt the generic bias applies as is.
        let mut options = whisper(None);
        assert!(merge(bias(&[], Some("p")), true, true, &mut options));
        assert_eq!(options.prompt.as_deref(), Some("p"));
        let mut options = RunOptions::default();
        assert!(merge(bias(&["Rust"], None), true, false, &mut options));
        assert_eq!(options.vocabulary, ["Rust"]);
    }

    #[test]
    fn rejected_biased_run_retries_once_without_bias() {
        let biased = RunOptions {
            vocabulary: vec!["Rust".into()],
            ..Default::default()
        };
        let unbiased = RunOptions::default();
        let mut seen = Vec::new();
        let result = run(&biased, Some(&unbiased), None, |o| {
            seen.push(o.vocabulary.len());
            if o.vocabulary.is_empty() {
                Ok("text")
            } else {
                Err(transcribe_cpp::Error::InvalidArgument("clash".into()))
            }
        });
        assert_eq!(result.ok(), Some("text"));
        assert_eq!(seen, [1, 0]);

        // Other errors, or nothing to fall back to, are returned unchanged.
        let mut calls = 0;
        let result: transcribe_cpp::Result<()> = run(&biased, Some(&unbiased), None, |_| {
            calls += 1;
            Err(transcribe_cpp::Error::InputTooLong("long".into()))
        });
        assert!(result.is_err() && calls == 1);
        let mut calls = 0;
        let result: transcribe_cpp::Result<()> = run(&biased, None, None, |_| {
            calls += 1;
            Err(transcribe_cpp::Error::InvalidArgument("bad".into()))
        });
        assert!(result.is_err() && calls == 1);
    }

    #[test]
    fn sanitize_strips_control_chars_and_special_token_literals() {
        assert_eq!(sanitize("a\0b\u{7}c"), "a b c");
        assert_eq!(sanitize("x <|endoftext|> y <|de|>z"), "x  y z");
        assert_eq!(sanitize("keep < | > and a<|b"), "keep < | > and ab");
        let context = DictationContext {
            window_title: Some("Chat <|startoftranscript|>\0Room".into()),
            ..Default::default()
        };
        assert_eq!(
            context_prompt(&context).as_deref(),
            Some("Window: Chat Room.")
        );
        assert_eq!(
            vocabulary(&["<|en|>".into(), "Ru\0st".into()], &[]),
            ["Ru st"]
        );
    }

    #[test]
    fn support_is_recorded_per_model() {
        record_support("asr-bias-test-a", true);
        record_support("asr-bias-test-b", false);
        record_support("asr-bias-test-a", false);
        assert_eq!(get_asr_bias_support("asr-bias-test-a".into()), Some(false));
        assert_eq!(get_asr_bias_support("asr-bias-test-b".into()), Some(false));
        assert_eq!(get_asr_bias_support("asr-bias-test-c".into()), None);
    }
}
