//! fork(voice-control): language-allowlist guard for "auto" detection.
//!
//! A model left to detect the language itself (Parakeet's per-utterance LID,
//! Qwen3-ASR's built-in LID) occasionally tags a short utterance with a
//! language the user never dictates (e.g. English mistaken for Russian, which
//! prints Cyrillic). When the run had no language hint, the allowlist is
//! non-empty and the result is out of bounds, the same audio is transcribed
//! once more: with the configured fallback model when one is set, downloaded
//! and different from the active model (no pin-retry after it, no cascade),
//! otherwise with the active model pinned to the first allowlisted language.
//! "Out of bounds" is a reported detected language outside the list, or —
//! for archs that never report one — an unsupported script (see
//! [`script_outside_allowlist`]). An empty allowlist or an in-bounds result
//! keeps the first transcript (upstream behaviour).
//!
//! Fork-owned: a child module of `managers::transcription`, so it reuses the
//! transcription's run setup and engine without widening upstream's types.
//! Independent of upstream's spoken-language detection (`language_detection`):
//! a run that detection pinned to a language carries a hint and is never
//! guarded here.

use log::{info, warn};
use tauri::Emitter;

use super::{
    echoes_vocabulary, resolve_gpu_device, run_setup, select_transcribe_backend,
    transcribe_cpp_run_plan, transcribe_gpu_disabled_for_host, BatchContext, RunOutcome,
    TranscriptionManager,
};
use crate::engine_supervisor::{DeviceSelector, EngineSupervisor, LoadSpec};
use crate::language_detection::{LanguageDecision, LanguageSource};
use crate::settings::AppSettings;
use transcribe_cpp::Backend;

/// Whether the guard may need the recording again (so the caller keeps a copy
/// of the audio).
pub(super) fn wants_audio(settings: &AppSettings) -> bool {
    !settings.language_allowlist.is_empty()
}

/// Check `outcome` against the allowlist and return what the transcription
/// keeps: the original, the fallback model's text, or the pinned retry.
pub(super) fn guard(
    tm: &TranscriptionManager,
    settings: &AppSettings,
    mut outcome: RunOutcome,
    audio: Vec<f32>,
) -> RunOutcome {
    let allowlist = &settings.language_allowlist;
    // Only "auto": a run the model was told a language for is in bounds by
    // construction.
    if allowlist.is_empty() || outcome.setup.applied_language_hint.is_some() {
        return outcome;
    }
    let mismatch = match outcome.model_detected_language.as_deref() {
        Some(detected) => {
            let detected_norm = normalize_lang_subtag(detected);
            allowlist
                .iter()
                .all(|a| normalize_lang_subtag(a) != detected_norm)
                .then(|| format!("detected language '{detected}'"))
        }
        None => script_outside_allowlist(&outcome.text, allowlist)
            .map(|script| format!("dominant {script} script")),
    };
    let Some(reason) = mismatch else {
        return outcome;
    };

    let active_model = tm
        .get_current_model()
        .unwrap_or_else(|| settings.selected_model.clone());
    // Fallback-model escalation: skip the same-engine pin-retry entirely — on
    // archs like parakeet the pin is only soft conditioning and cannot fix a
    // genuine misdetection.
    let fallback_model = settings
        .language_allowlist_fallback_model
        .as_deref()
        .filter(|id| !id.is_empty() && *id != active_model)
        .filter(|id| {
            tm.model_manager
                .get_model_info(id)
                .is_some_and(|m| m.is_downloaded)
        });
    crate::journal::record_allowlist_guard(&reason, allowlist, fallback_model, &outcome.text);

    if let Some(fallback_model) = fallback_model {
        info!(
            "Language allowlist guard: {} outside {:?}, escalating to fallback model '{}'",
            reason, allowlist, fallback_model
        );
        // Announce the second pass so the overlay can explain the latency.
        let _ = tm.app_handle.emit("fallback-transcription", ());
        if let Some(text) = run_fallback(tm, fallback_model, audio, settings) {
            // The fallback model produced this text, so the primary run's
            // language evidence no longer applies; text-based detection
            // decides.
            outcome.text = text;
            outcome.model_detected_language = None;
            outcome.setup.applied_language_hint = None;
            outcome.setup.output_was_translated = false;
            crate::journal::record_allowlist_result(
                crate::journal::AllowlistResult::FallbackAccepted,
                Some(&outcome.text),
            );
        } else {
            crate::journal::record_allowlist_result(
                crate::journal::AllowlistResult::FallbackRejected,
                None,
            );
        }
        return outcome;
    }

    let retry_lang = allowlist[0].clone();
    let Some(info) = tm.engine.loaded() else {
        return outcome;
    };
    // The shared plan forwards the pin only when the model advertises it;
    // otherwise the retry would auto-detect again and reproduce the result.
    let plan = transcribe_cpp_run_plan(
        settings.translate_to_english,
        &retry_lang,
        &info.capabilities.languages,
        info.capabilities.supports_translate,
    );
    if plan.language.is_none() {
        info!(
            "Language allowlist guard: '{}' not advertised by the model; keeping original detection",
            retry_lang
        );
        crate::journal::record_allowlist_result(
            crate::journal::AllowlistResult::PinUnsupported,
            None,
        );
        return outcome;
    }
    info!(
        "Language allowlist guard: {} outside {:?}, re-running pinned to '{}'",
        reason, allowlist, retry_lang
    );
    let setup = run_setup(&info, settings, pinned(&retry_lang));
    let batch = BatchContext {
        info,
        settings: settings.clone(),
        active_model,
        setup,
    };
    match tm.transcribe_cpp(tm.engine.cancel_mark(), batch, audio) {
        Ok(retried) => {
            // The pin is only a soft conditioning on some archs (parakeet);
            // say so when the model ignored it instead of a hollow "succeeded".
            if script_outside_allowlist(&retried.text, allowlist).is_some() {
                warn!(
                    "Language allowlist guard: retry pinned to '{}' still out of script; the model cannot honor the pin for this utterance",
                    retry_lang
                );
            } else {
                info!(
                    "Language allowlist guard: retry pinned to '{}' succeeded",
                    retry_lang
                );
            }
            crate::journal::record_allowlist_result(
                crate::journal::AllowlistResult::PinRetryOk,
                Some(&retried.text),
            );
            retried
        }
        Err(e) => {
            warn!(
                "Language allowlist guard: retry pinned to '{}' failed ({}); keeping original detection",
                retry_lang, e
            );
            crate::journal::record_allowlist_result(
                crate::journal::AllowlistResult::PinRetryFailed,
                None,
            );
            outcome
        }
    }
}

/// The allowlist's primary language as the run's language.
fn pinned(language: &str) -> LanguageDecision {
    LanguageDecision {
        language: Some(language.to_string()),
        source: LanguageSource::Fallback,
    }
}

/// Transcribe `audio` once with `fallback_model` in its own, throwaway
/// transcription worker, and return its text only when it is in bounds. The
/// worker (a second model in memory, briefly) exits when the scoped engine is
/// dropped; the guard fires rarely, so the active model is never swapped out.
fn run_fallback(
    tm: &TranscriptionManager,
    fallback_model: &str,
    audio: Vec<f32>,
    settings: &AppSettings,
) -> Option<String> {
    let path = match tm.model_manager.get_model_path(fallback_model) {
        Ok(path) => path,
        Err(e) => {
            warn!(
                "Language allowlist guard: fallback model '{}' is unavailable ({}); keeping original detection",
                fallback_model, e
            );
            return None;
        }
    };
    let (backend, device) = match resolve_gpu_device(
        settings.transcribe_accelerator,
        settings.transcribe_gpu_device.as_deref(),
    ) {
        Some(key) => (Backend::Auto, DeviceSelector::Key(key)),
        None => (
            select_transcribe_backend(settings.transcribe_accelerator),
            DeviceSelector::Auto,
        ),
    };
    let engine = EngineSupervisor::new(!transcribe_gpu_disabled_for_host());
    let info = match engine.load(LoadSpec {
        path,
        backend,
        device,
    }) {
        Ok(info) => info,
        Err(e) => {
            warn!(
                "Language allowlist guard: fallback model '{}' failed to load ({}); keeping original detection",
                fallback_model, e
            );
            return None;
        }
    };
    // The fallback's own settings view, so its run setup (vocabulary, bias
    // support) is keyed to the fallback model, not the selected one.
    let mut fallback_settings = settings.clone();
    fallback_settings.selected_model = fallback_model.to_string();
    let setup = run_setup(
        &info,
        &fallback_settings,
        pinned(&settings.language_allowlist[0]),
    );
    let result = engine.transcribe(audio, setup.run_options.clone());
    // Dropping the last handle ends the scoped worker off this thread.
    drop(engine);

    let transcript = match result {
        Ok(transcript) => transcript,
        Err(e) => {
            warn!(
                "Language allowlist guard: fallback model '{}' run failed ({}); keeping original detection",
                fallback_model, e
            );
            return None;
        }
    };
    if echoes_vocabulary(&transcript.text, &setup.run_options.vocabulary) {
        warn!(
            "Language allowlist guard: fallback model '{}' returned the custom words; keeping original detection",
            fallback_model
        );
        return None;
    }
    if fallback_output_in_bounds(
        &transcript.text,
        transcript.language.as_deref(),
        &settings.language_allowlist,
    ) {
        info!(
            "Language allowlist guard: fallback model '{}' rescued the utterance (detected {:?})",
            fallback_model, transcript.language
        );
        Some(transcript.text)
    } else {
        warn!(
            "Language allowlist guard: fallback model '{}' output still out of bounds; keeping original detection",
            fallback_model
        );
        None
    }
}

/// Normalize an ISO language tag for allowlist comparison: lowercase and
/// primary subtag only ("de-DE" -> "de").
fn normalize_lang_subtag(code: &str) -> String {
    code.split('-').next().unwrap_or(code).to_ascii_lowercase()
}

/// Writing-system buckets for the guard's script fallback, used when the
/// model reports no detected language (parakeet never does).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Script {
    Latin,
    Cyrillic,
    Greek,
    Hebrew,
    Arabic,
    Cjk,
    Hangul,
}

const SCRIPTS: [Script; 7] = [
    Script::Latin,
    Script::Cyrillic,
    Script::Greek,
    Script::Hebrew,
    Script::Arabic,
    Script::Cjk,
    Script::Hangul,
];

impl Script {
    fn name(self) -> &'static str {
        match self {
            Script::Latin => "Latin",
            Script::Cyrillic => "Cyrillic",
            Script::Greek => "Greek",
            Script::Hebrew => "Hebrew",
            Script::Arabic => "Arabic",
            Script::Cjk => "CJK",
            Script::Hangul => "Hangul",
        }
    }
}

/// The script a language (primary subtag) is written in. Covers the languages
/// Handy's model catalog advertises; unknown codes fall back to Latin, which
/// keeps the guard from ever retrying Latin output of an unmapped language.
fn script_for_language(lang: &str) -> Script {
    match normalize_lang_subtag(lang).as_str() {
        "ru" | "uk" | "bg" | "be" | "sr" | "mk" | "kk" => Script::Cyrillic,
        "el" => Script::Greek,
        "he" | "yi" => Script::Hebrew,
        "ar" | "fa" | "ur" => Script::Arabic,
        "zh" | "yue" | "ja" => Script::Cjk,
        "ko" => Script::Hangul,
        _ => Script::Latin,
    }
}

/// The bucket of one script-carrying character; None for anything the guard
/// ignores (digits, punctuation, whitespace, symbols).
fn char_script(c: char) -> Option<Script> {
    match c as u32 {
        // ASCII letters plus Latin-1/Extended (umlauts, accents, …).
        0x0041..=0x005A | 0x0061..=0x007A | 0x00C0..=0x024F => Some(Script::Latin),
        0x0370..=0x03FF => Some(Script::Greek),
        0x0400..=0x052F => Some(Script::Cyrillic),
        0x0590..=0x05FF => Some(Script::Hebrew),
        0x0600..=0x06FF | 0x0750..=0x077F => Some(Script::Arabic),
        // Kana + CJK unified ideographs (incl. extension A).
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF => Some(Script::Cjk),
        0x1100..=0x11FF | 0xAC00..=0xD7AF => Some(Script::Hangul),
        _ => None,
    }
}

/// Reject an unsupported dominant script (>60%, >=4 letters), or a token
/// containing >=3 letters of an unsupported non-Latin script. This
/// intentionally catches Cyrillic names inside otherwise German text; isolated
/// Greek math letters remain below the token threshold. Latin tokens never
/// count for the token rule: brand names and code ("Открой Zoom") are Latin in
/// every language, so only the dominant rule judges Latin.
fn script_outside_allowlist(text: &str, allowlist: &[String]) -> Option<&'static str> {
    const MIN_CHARS: usize = 4;
    const MIN_TOKEN_LETTERS: usize = 3;
    let allowed = |script| {
        allowlist
            .iter()
            .any(|lang| script_for_language(lang) == script)
    };
    let mut counts = [0usize; SCRIPTS.len()];
    let mut total = 0usize;
    for token in text.split_whitespace() {
        let mut token_counts = [0usize; SCRIPTS.len()];
        for c in token.chars().filter(|c| c.is_alphabetic()) {
            if let Some(script) = char_script(c) {
                let idx = SCRIPTS.iter().position(|&s| s == script)?;
                counts[idx] += 1;
                token_counts[idx] += 1;
                total += 1;
            }
        }
        for (idx, &n) in token_counts.iter().enumerate() {
            if n >= MIN_TOKEN_LETTERS && SCRIPTS[idx] != Script::Latin && !allowed(SCRIPTS[idx]) {
                return Some(SCRIPTS[idx].name());
            }
        }
    }
    if total < MIN_CHARS {
        return None;
    }
    let (idx, &n) = counts.iter().enumerate().max_by_key(|(_, &n)| n)?;
    // Majority check: n / total > 0.6.
    if n * 5 <= total * 3 || allowed(SCRIPTS[idx]) {
        return None;
    }
    Some(SCRIPTS[idx].name())
}

/// Fallback output must pass the same script predicate, even when the model
/// reports an allowlisted language.
fn fallback_output_in_bounds(
    text: &str,
    detected_language: Option<&str>,
    allowlist: &[String],
) -> bool {
    if script_outside_allowlist(text, allowlist).is_some() {
        return false;
    }
    match detected_language {
        Some(detected) => {
            let detected_norm = normalize_lang_subtag(detected);
            allowlist
                .iter()
                .any(|a| normalize_lang_subtag(a) == detected_norm)
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn languages(codes: &[&str]) -> Vec<String> {
        codes.iter().map(|code| (*code).to_string()).collect()
    }

    #[test]
    fn cyrillic_output_against_latin_allowlist_is_flagged() {
        // The real-world failure: an English phrase transcribed in Cyrillic.
        let text = "Фит слэш ватсап даш лайфсайкл.";
        assert_eq!(
            script_outside_allowlist(text, &languages(&["de", "en"])),
            Some("Cyrillic")
        );
    }

    #[test]
    fn latin_output_with_umlauts_is_in_bounds_for_german() {
        let text = "Größenordnung geändert, äußerst zügig!";
        assert_eq!(
            script_outside_allowlist(text, &languages(&["de", "en"])),
            None
        );
    }

    #[test]
    fn cyrillic_output_is_fine_when_russian_is_allowlisted() {
        let text = "Привет как дела";
        assert_eq!(
            script_outside_allowlist(text, &languages(&["ru", "de"])),
            None
        );
    }

    #[test]
    fn short_tokens_and_mixed_output_without_three_unsupported_letters_are_in_bounds() {
        // Below both the dominant and token thresholds.
        assert_eq!(script_outside_allowlist("Да!", &languages(&["de"])), None);
        // No clear (>60%) majority.
        assert_eq!(
            script_outside_allowlist("Hello Пр hi ве да", &languages(&["de"])),
            None
        );
    }

    // Token-level script guard regressions.
    #[test]
    fn unsupported_letters_inside_a_token_trigger_without_a_dominant_script() {
        for text in [
            "Schreib an mark этеxampelpunkt com.",
            "Schreib bitte an Иван heute.",
            "Привет",
            "abэ1т!е🙂cd",
        ] {
            assert_eq!(
                script_outside_allowlist(text, &languages(&["de"])),
                Some("Cyrillic")
            );
        }
        assert_eq!(
            script_outside_allowlist("Schreib an Иван", &languages(&["de", "ru"])),
            None
        );
    }

    // Symbols inside Unicode script ranges are not letters.
    #[test]
    fn math_letters_symbols_digits_and_emoji_do_not_false_positive() {
        for text in [
            "Winkel α beträgt",
            "αβ",
            "🙂🚀✨",
            "1234 ×÷×÷",
            "١٢٣٤",
            "α1!β🙂",
        ] {
            assert_eq!(script_outside_allowlist(text, &languages(&["de"])), None);
        }
        // Dominant rule still catches four unsupported letters across short tokens.
        assert_eq!(
            script_outside_allowlist("α β γ δ", &languages(&["de"])),
            Some("Greek")
        );
        // An exact 60% majority with short tokens still stays below the dominant rule.
        assert_eq!(
            script_outside_allowlist("αβ γ ab", &languages(&["de"])),
            None
        );
    }

    // Latin names inside non-Latin text never trigger the token rule; only a
    // Latin majority does.
    #[test]
    fn latin_tokens_are_exempt_from_the_token_rule() {
        let ru = languages(&["ru"]);
        assert_eq!(script_outside_allowlist("Открой Zoom", &ru), None);
        assert_eq!(
            script_outside_allowlist("Открой Zoom и Telegram сейчас", &ru),
            None
        );
        assert!(fallback_output_in_bounds("Открой Zoom", Some("ru"), &ru));
        assert!(fallback_output_in_bounds("Открой Zoom", None, &ru));
        // The dominant rule still rejects mostly-Latin output.
        assert_eq!(
            script_outside_allowlist("Open the Zoom app now", &ru),
            Some("Latin")
        );
        assert!(!fallback_output_in_bounds(
            "Open the Zoom app now",
            None,
            &ru
        ));
    }

    // A claimed allowed LID cannot hide an unsupported token.
    #[test]
    fn fallback_rejects_unsupported_tokens_with_or_without_allowed_lid() {
        let text = "Schreib an mark этеxampelpunkt com.";
        for detected in [Some("de"), None] {
            assert!(!fallback_output_in_bounds(
                text,
                detected,
                &languages(&["de", "en"])
            ));
        }
        assert!(fallback_output_in_bounds(
            "Winkel α beträgt",
            Some("de"),
            &languages(&["de"])
        ));
    }

    #[test]
    fn fallback_output_accepted_when_detected_language_is_allowlisted() {
        // Reported detected language inside the allowlist (region subtag ignored).
        assert!(fallback_output_in_bounds(
            "Feed slash WhatsApp dash lifecycle",
            Some("en-US"),
            &languages(&["de", "en"]),
        ));
    }

    #[test]
    fn fallback_output_rejected_when_detected_language_is_out_of_bounds() {
        assert!(!fallback_output_in_bounds(
            "Привет как дела",
            Some("ru"),
            &languages(&["de", "en"]),
        ));
    }

    #[test]
    fn fallback_output_falls_back_to_script_when_no_language_reported() {
        // Parakeet-style: no detected language, so the dominant script decides.
        assert!(fallback_output_in_bounds(
            "Feed slash WhatsApp dash lifecycle",
            None,
            &languages(&["de", "en"]),
        ));
        assert!(!fallback_output_in_bounds(
            "Фит слэш ватсап даш лайфсайкл.",
            None,
            &languages(&["de", "en"]),
        ));
    }
}
