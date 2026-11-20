//! Trigger-gated LLM disfluency cleanup (fork feature: voice-control) — Wispr
//! Flow's "backtrack" and filler cleanup: "um drei, nein warte, um vier" →
//! "um vier", "morgen, äh, übermorgen" → "übermorgen", "ist ist" → "ist".
//!
//! An LLM pass runs **only** when the transcript contains a disfluency
//! ([`disfluency::detect`]: a filler, a stutter, an aborted start or a
//! self-correction cue); every other dictation is pasted as the rules
//! produced it — no latency, no over-editing. The pass is switched by
//! `self_correction_llm_enabled` (default on). Remote processing additionally
//! requires upstream's `post_process_enabled` privacy opt-in.
//!
//! Provider: Apple Intelligence on-device when available, else the user's
//! configured post-process provider if remote processing is enabled and it has
//! a model and an API key, else the pass is skipped. Apple runs on a
//! dedicated session ([`apple_session`]) prepared when a recording starts so
//! it is warm by the time the transcript arrives. The call is capped at
//! [`TIMEOUT`] ([`COLD_TIMEOUT`] while the Apple session is still warming up);
//! any failure, timeout or over-edit ([`guard::check`]: only
//! deletions at the detected spans) keeps the rules output. Every dictation
//! with a disfluency is journaled as
//! `self_correction {requested, applied, ms, reason, cue, provider}`, `cue`
//! holding the detected kinds (`filler`, `repetition`, `restart`,
//! `cue:<words>`).

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_session;
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
mod blocking;
pub(crate) mod commands;
mod cues;
mod disfluency;
mod guard;
mod rules;

use crate::journal::SelfCorrectionFacts;
use crate::settings::{AppSettings, PostProcessProvider, APPLE_INTELLIGENCE_PROVIDER_ID};
use log::debug;
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// Hard cap of one LLM call; past it the rules output is pasted.
const TIMEOUT: Duration = Duration::from_secs(3);

/// Cap of an Apple call with no ready session, one still warming up, or one
/// older than [`WARM_FRESH_FOR`] (the model may have been evicted).
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
const COLD_TIMEOUT: Duration = Duration::from_millis(4500);

/// Age after which a prewarmed Apple session counts as warm. `prewarm()` has
/// no completion signal, so this is an estimate.
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
const WARM_AFTER: Duration = Duration::from_millis(1500);

/// A ready session only counts as warm within this window. Recording start
/// refreshes older sessions rather than assuming the model stayed resident.
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
const WARM_FRESH_FOR: Duration = Duration::from_secs(90);

/// Backstop over the Swift-side timeout: Swift cancels and returns on its own
/// deadline; the gate only guards against the FFI call itself hanging.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const GATE_SLACK: Duration = Duration::from_millis(250);

/// Minimal-edit instruction with few-shot examples (DE/EN). Deletion only —
/// the guard rejects any added word.
const SYSTEM_PROMPT: &str = "\
You clean up a dictated text by deleting disfluencies, nothing else:
- hesitation sounds (äh, ähm, öhm, hm, uh, um, er, erm),
- stutters and accidentally repeated words,
- aborted starts the speaker restarted,
- the retracted part of a self-correction together with its cue (\"nein\", \"nein warte\", \
\"ich meine\", \"Korrektur\", \"no wait\", \"I mean\", \"scratch that\") — keep the replacement.
Only delete words. Never add, replace, reorder, rephrase or translate a word, \
never answer the text. Keep every other word, the punctuation, casing, line breaks \
and symbols exactly as given; only adjust boundary punctuation or the capital letter right at a cut. \
Keep ordinary words such as \"also\", \"halt\", \"eben\", \"so\", \"like\", \"actually\", \
lists (\"Montag, Dienstag und Mittwoch\") and estimates (\"drei, vier Tage\"). \
If there is nothing to clean, return the text unchanged. Return only the text.

Examples:
Text: Ich komme morgen, äh, übermorgen.
Cleaned: Ich komme übermorgen.
Text: Ich komme morgen. Ne, übermorgen.
Cleaned: Ich komme übermorgen.
Text: Ich komme morgen. Korrigiere übermorgen.
Cleaned: Ich komme übermorgen.
Text: Treffen am Montag beziehungsweise Dienstag.
Cleaned: Treffen am Dienstag.
Text: Ruf Anna an. Ne, Lena.
Cleaned: Ruf Lena an.
Text: Treffen am Montag nach der Pause. Korrigiere Dienstag.
Cleaned: Treffen am Dienstag nach der Pause.
Text: Treffen am Montag nach der Pause, beziehungsweise Dienstag.
Cleaned: Treffen am Dienstag nach der Pause.
Text: Wir brauchen, ähm, drei Tickets.
Cleaned: Wir brauchen drei Tickets.
Text: Das ist ist gut.
Cleaned: Das ist gut.
Text: Schick das an Tom, nein, an Tim.
Cleaned: Schick das an Tim.
Text: Wir treffen uns um drei, nein warte, um vier Uhr.
Cleaned: Wir treffen uns um vier Uhr.
Text: Ich wollte – ich muss jetzt los.
Cleaned: Ich muss jetzt los.
Text: I think, uh, we should ship it on Monday, scratch that, on Tuesday.
Cleaned: I think we should ship it on Tuesday.
Text: Ich meine, das ist eine gute Idee.
Cleaned: Ich meine, das ist eine gute Idee.";

/// The LLM backend chosen for one pass.
#[derive(Debug, Clone)]
enum Provider {
    Apple,
    Remote {
        provider: PostProcessProvider,
        api_key: String,
        model: String,
    },
}

impl Provider {
    fn id(&self) -> String {
        match self {
            Provider::Apple => APPLE_INTELLIGENCE_PROVIDER_ID.to_string(),
            Provider::Remote { provider, .. } => provider.id.clone(),
        }
    }
}

/// Pure privacy policy: only Apple may run without remote-processing opt-in.
fn choose_provider(
    settings: &AppSettings,
    apple_available: bool,
) -> Result<Provider, &'static str> {
    if apple_available {
        return Ok(Provider::Apple);
    }
    if !settings.post_process_enabled {
        return Err("no_local_provider");
    }
    if settings.post_process_provider_id == APPLE_INTELLIGENCE_PROVIDER_ID {
        return Err("apple_unavailable");
    }
    let Some(provider) = settings.active_post_process_provider() else {
        return Err("no_provider");
    };
    let model = settings
        .post_process_models
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    let api_key = settings
        .post_process_api_keys
        .get(&provider.id)
        .cloned()
        .unwrap_or_default();
    if model.trim().is_empty() || api_key.trim().is_empty() {
        return Err("no_provider");
    }
    Ok(Provider::Remote {
        provider: provider.clone(),
        api_key,
        model,
    })
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
fn apple_gate() -> &'static blocking::BlockingGate {
    static GATE: OnceLock<blocking::BlockingGate> = OnceLock::new();
    GATE.get_or_init(blocking::BlockingGate::default)
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
async fn apple_available() -> Result<bool, &'static str> {
    // Availability also enters Swift/FFI and may block. Share the call gate
    // so a still-running timed-out request makes subsequent passes skip busy.
    apple_gate().run(TIMEOUT, apple_session::available).await
}

#[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
async fn apple_available() -> Result<bool, &'static str> {
    Ok(false)
}

/// Warm the on-device session at recording start, refreshing stale sessions.
/// Non-blocking and idempotent within the freshness window; a no-op unless enabled
/// and would pick Apple.
pub(crate) fn prepare(settings: &AppSettings) {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        if !settings.self_correction_llm_enabled {
            return;
        }
        let settings = settings.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let available = apple_session::available();
            if matches!(choose_provider(&settings, available), Ok(Provider::Apple)) {
                apple_session::prepare(SYSTEM_PROMPT);
            }
        });
    }
    #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
    let _ = settings;
}

/// Warm sessions use the short cap only while their prewarm is fresh.
#[cfg(any(test, all(target_os = "macos", target_arch = "aarch64")))]
fn apple_budget(age: Option<Duration>) -> Duration {
    if age.is_some_and(|age| (WARM_AFTER..=WARM_FRESH_FOR).contains(&age)) {
        TIMEOUT
    } else {
        COLD_TIMEOUT
    }
}

/// Total budget of one pass with `provider`, counted from detection.
fn budget(provider: &Provider) -> Duration {
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    if matches!(provider, Provider::Apple) {
        let age = apple_session::prepared_age();
        let timeout = apple_budget(age);
        debug!(
            "self-correction: Apple session prepared {} → {}",
            age.map_or("never".to_string(), |age| format!(
                "{} ms ago",
                age.as_millis()
            )),
            if timeout == TIMEOUT { "warm" } else { "cold" }
        );
        return timeout;
    }
    let _ = provider;
    TIMEOUT
}

/// One LLM call; `Err` carries the journal reason.
async fn call(provider: &Provider, text: &str, timeout: Duration) -> Result<String, &'static str> {
    match provider {
        Provider::Apple => {
            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            {
                use apple_session::RunError;
                let text = text.to_string();
                let run_started = Instant::now();
                // Swift cancels the generation at `timeout` and returns; the
                // worker retains the gate until then, and a late result is
                // discarded and cannot edit a later pass.
                let result = apple_gate()
                    .run(timeout + GATE_SLACK, move || {
                        // A no-op when the recording hook prepared a fresh session.
                        apple_session::prepare(SYSTEM_PROMPT);
                        apple_session::run(&text, timeout)
                    })
                    .await;
                debug!(
                    "self-correction: Apple run {} ms (cap {} ms)",
                    run_started.elapsed().as_millis(),
                    timeout.as_millis()
                );
                match result {
                    Ok(Ok(result)) => Ok(result),
                    Ok(Err(err)) => {
                        if let RunError::Failed(ref message) = err {
                            debug!("self-correction: Apple Intelligence failed: {}", message);
                        }
                        Err(err.reason())
                    }
                    Err(reason) => Err(reason),
                }
            }
            #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
            {
                let _ = text;
                Err("apple_unavailable")
            }
        }
        Provider::Remote {
            provider,
            api_key,
            model,
        } => {
            let request = crate::llm_client::send_chat_completion_with_schema(
                provider,
                api_key.clone(),
                model,
                text.to_string(),
                Some(SYSTEM_PROMPT.to_string()),
                None,
                matches!(provider.id.as_str(), "custom" | "openrouter"),
            );
            match tokio::time::timeout(timeout, request).await {
                Err(_) => Err("timeout"),
                Ok(Ok(Some(content))) => Ok(content),
                Ok(Ok(None)) => Err("empty"),
                Ok(Err(err)) => {
                    debug!(
                        "self-correction: provider '{}' failed: {}",
                        provider.id, err
                    );
                    Err("error")
                }
            }
        }
    }
}

/// Run the disfluency-cleanup pass on `text` (the transcription result after
/// learned corrections / upstream post-processing). Returns the text to keep
/// and, when a disfluency was found, the journal facts.
///
/// `skip` names a reason the pass must not edit this text (a snippet
/// expansion is in it, upstream's LLM already rewrote it).
pub(crate) async fn apply(
    settings: &AppSettings,
    text: String,
    skip: Option<&'static str>,
) -> (String, Option<SelfCorrectionFacts>) {
    let lang = crate::correction_learning::last_transcription_language();
    apply_in_language(settings, text, skip, lang.as_deref()).await
}

/// [`apply`] with an explicit transcription language (en-only fillers such as
/// "um" depend on it).
async fn apply_in_language(
    settings: &AppSettings,
    text: String,
    skip: Option<&'static str>,
    lang: Option<&str>,
) -> (String, Option<SelfCorrectionFacts>) {
    let started = Instant::now();
    let spans = disfluency::detect(&text, lang);
    if spans.is_empty() {
        return (text, None);
    }
    let mut facts = SelfCorrectionFacts {
        requested: settings.self_correction_llm_enabled,
        cue: kinds(&spans),
        ..Default::default()
    };
    debug!(
        "self-correction: detected '{}' in {} µs",
        facts.cue,
        started.elapsed().as_micros()
    );
    let finish = |mut facts: SelfCorrectionFacts, reason: &str| {
        facts.reason = reason.to_string();
        facts.ms = started.elapsed().as_millis() as u64;
        debug!(
            "self-correction: '{}' → {} ({} ms, provider {})",
            facts.cue,
            facts.reason,
            facts.ms,
            facts.provider.as_deref().unwrap_or("-")
        );
        facts
    };

    if !settings.self_correction_llm_enabled {
        return (text, Some(finish(facts, "disabled")));
    }
    if let Some(reason) = skip {
        return (text, Some(finish(facts, reason)));
    }
    if let Some(repaired) = rules::repair(&text, &spans) {
        facts.provider = Some("rules".to_string());
        facts.applied = true;
        return (repaired, Some(finish(facts, "applied")));
    }
    let available = match apple_available().await {
        Ok(available) => available,
        Err(reason) => return (text, Some(finish(facts, reason))),
    };
    let provider = match choose_provider(settings, available) {
        Ok(provider) => provider,
        Err(reason) => return (text, Some(finish(facts, reason))),
    };
    facts.provider = Some(provider.id());

    let remaining = budget(&provider).saturating_sub(started.elapsed());
    if remaining.is_zero() {
        return (text, Some(finish(facts, "timeout")));
    }
    let raw = match call(&provider, &text, remaining).await {
        Ok(raw) => raw,
        Err(reason) => return (text, Some(finish(facts, reason))),
    };
    let (out, reason) = evaluate_candidate(text, &raw, &spans, &mut facts);
    (out, Some(finish(facts, reason)))
}

/// Capture rejected model text only in journal facts; never log its content.
fn evaluate_candidate(
    text: String,
    raw: &str,
    spans: &[disfluency::Span],
    facts: &mut SelfCorrectionFacts,
) -> (String, &'static str) {
    let candidate = guard::clean_response(&text, raw);
    match guard::check(&text, &candidate, spans) {
        Ok(()) => {
            facts.applied = true;
            (candidate, "applied")
        }
        Err(reason) => {
            facts.candidate = Some(candidate);
            (text, reason)
        }
    }
}

/// The journal's `cue`: the distinct detected kinds, cues first, then in
/// text order.
fn kinds(spans: &[disfluency::Span]) -> String {
    let mut kinds: Vec<&str> = Vec::new();
    for span in spans {
        if !kinds.contains(&span.label.as_str()) {
            kinds.push(&span.label);
        }
    }
    kinds.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings_with(provider: &str, model: &str, key: &str) -> AppSettings {
        let mut settings = crate::settings::get_default_settings();
        settings.post_process_enabled = true;
        settings.post_process_provider_id = provider.to_string();
        settings
            .post_process_models
            .insert(provider.to_string(), model.to_string());
        settings
            .post_process_api_keys
            .insert(provider.to_string(), key.to_string());
        settings
    }

    #[test]
    fn apple_budget_requires_a_fresh_session_with_time_to_warm_up() {
        for age in [
            None,
            Some(Duration::ZERO),
            Some(WARM_AFTER - Duration::from_millis(1)),
            Some(WARM_FRESH_FOR + Duration::from_millis(1)),
            Some(Duration::from_millis(813823)),
        ] {
            assert_eq!(apple_budget(age), COLD_TIMEOUT, "{age:?}");
        }
        for age in [WARM_AFTER, Duration::from_secs(30), WARM_FRESH_FOR] {
            assert_eq!(apple_budget(Some(age)), TIMEOUT, "{age:?}");
        }
    }

    #[test]
    fn apple_intelligence_is_preferred_when_available() {
        let settings = settings_with("openai", "gpt-4o-mini", "sk-test");
        assert!(matches!(
            choose_provider(&settings, true),
            Ok(Provider::Apple)
        ));
    }

    #[test]
    fn falls_back_to_a_configured_provider_with_a_key() {
        let settings = settings_with("openai", "gpt-4o-mini", "sk-test");
        let provider = choose_provider(&settings, false).expect("provider");
        assert_eq!(provider.id(), "openai");
    }

    #[test]
    fn skips_without_a_usable_provider() {
        // The user's setup: openai selected, no key.
        let settings = settings_with("openai", "gpt-4o-mini", "");
        assert_eq!(choose_provider(&settings, false).err(), Some("no_provider"));
        let settings = settings_with(APPLE_INTELLIGENCE_PROVIDER_ID, "Apple Intelligence", "");
        assert_eq!(
            choose_provider(&settings, false).err(),
            Some("apple_unavailable")
        );
    }

    #[test]
    fn disabled_post_processing_never_selects_remote_even_with_saved_credentials() {
        let mut settings = settings_with("openai", "gpt-4o-mini", "sk-test");
        settings.post_process_enabled = false;
        assert_eq!(
            choose_provider(&settings, false).err(),
            Some("no_local_provider")
        );
        assert!(matches!(
            choose_provider(&settings, true),
            Ok(Provider::Apple)
        ));
    }

    #[test]
    fn remote_opt_in_only_allows_the_selected_upstream_provider() {
        let mut settings = settings_with("openai", "gpt-4o-mini", "sk-test");
        settings.post_process_provider_id = "anthropic".to_string();
        assert_eq!(choose_provider(&settings, false).err(), Some("no_provider"));
        settings
            .post_process_models
            .insert("anthropic".into(), "selected-model".into());
        settings
            .post_process_api_keys
            .insert("anthropic".into(), "selected-key".into());
        match choose_provider(&settings, false).expect("selected remote") {
            Provider::Remote {
                provider,
                model,
                api_key,
            } => {
                assert_eq!(provider.id, "anthropic");
                assert_eq!(model, "selected-model");
                assert_eq!(api_key, "selected-key");
            }
            Provider::Apple => panic!("Apple unavailable"),
        }
    }

    #[test]
    fn disabled_post_processing_skips_without_local_even_for_an_unknown_provider() {
        let mut settings = settings_with("unknown-provider", "saved-model", "saved-key");
        settings.post_process_enabled = false;
        assert_eq!(
            choose_provider(&settings, false).err(),
            Some("no_local_provider")
        );
        settings.post_process_enabled = true;
        assert_eq!(choose_provider(&settings, false).err(), Some("no_provider"));
    }

    #[test]
    fn remote_requires_both_a_nonblank_model_and_key() {
        for (model, key) in [
            ("", "saved-key"),
            ("  ", "saved-key"),
            ("saved-model", ""),
            ("saved-model", "  "),
        ] {
            let settings = settings_with("openai", model, key);
            assert_eq!(choose_provider(&settings, false).err(), Some("no_provider"));
        }
    }

    #[test]
    fn no_disfluency_means_no_pass_and_no_facts() {
        let settings = crate::settings::get_default_settings();
        let text = "Wir treffen uns um vier Uhr.".to_string();
        let (out, facts) = tauri::async_runtime::block_on(apply_in_language(
            &settings,
            text.clone(),
            None,
            Some("de"),
        ));
        assert_eq!(out, text);
        assert!(facts.is_none());
    }

    #[test]
    fn slot_repairs_skip_provider_selection_and_inference() {
        let mut settings = crate::settings::get_default_settings();
        settings.post_process_enabled = false;
        for (input, expected) in [
            ("Ich komme morgen. Ne, übermorgen.", "Ich komme übermorgen."),
            (
                "Ich komme morgen. Korrigiere übermorgen.",
                "Ich komme übermorgen.",
            ),
            (
                "Treffen am Montag beziehungsweise Dienstag.",
                "Treffen am Dienstag.",
            ),
            ("Treffen am Montag bzw. Dienstag.", "Treffen am Dienstag."),
            (
                "Wir treffen uns um drei, nein warte, um vier.",
                "Wir treffen uns um vier.",
            ),
            ("Schick das an Tom. Nein an Tim.", "Schick das an Tim."),
            ("Ich komme morgen übermorgen.", "Ich komme übermorgen."),
            ("See you at five, make that six.", "See you at six."),
            (
                "Treffen am Montag, beziehungsweise Dienstag.",
                "Treffen am Dienstag.",
            ),
        ] {
            let (out, facts) = tauri::async_runtime::block_on(apply_in_language(
                &settings,
                input.to_string(),
                None,
                Some("de"),
            ));
            assert_eq!(out, expected, "{input}");
            let facts = facts.expect("rules facts");
            assert_eq!(facts.provider.as_deref(), Some("rules"));
            assert_eq!(facts.reason, "applied");
            assert!(facts.applied && facts.requested);
            assert!(facts.candidate.is_none());
        }
    }

    #[test]
    fn rejected_and_unchanged_candidates_are_journaled() {
        let input = "Ich komme morgen. Ne, übermorgen.";
        let spans = disfluency::detect(input, Some("de"));
        for (raw, expected_reason) in [
            (input, "unchanged"),
            ("Ich komme nächsten übermorgen.", "added_words"),
            ("Ich übermorgen.", "unrelated_deletion"),
        ] {
            let mut facts = SelfCorrectionFacts::default();
            let (out, reason) = evaluate_candidate(input.to_string(), raw, &spans, &mut facts);
            assert_eq!(out, input);
            assert_eq!(reason, expected_reason);
            assert_eq!(facts.candidate.as_deref(), Some(raw));
            assert!(!facts.applied);
            assert_eq!(serde_json::to_value(facts).unwrap()["candidate"], raw);
        }
        let mut facts = SelfCorrectionFacts::default();
        let (out, reason) = evaluate_candidate(
            input.to_string(),
            "Ich komme übermorgen.",
            &spans,
            &mut facts,
        );
        assert_eq!(out, "Ich komme übermorgen.");
        assert_eq!(reason, "applied");
        assert!(facts.applied);
        assert!(facts.candidate.is_none());
        assert!(serde_json::to_value(facts)
            .unwrap()
            .get("candidate")
            .is_none());
    }

    #[test]
    fn disabled_or_skipped_passes_keep_the_text() {
        let mut settings = crate::settings::get_default_settings();
        let text = "Um drei, nein warte, um vier.".to_string();
        let (out, facts) = tauri::async_runtime::block_on(apply_in_language(
            &settings,
            text.clone(),
            Some("snippet"),
            Some("de"),
        ));
        assert_eq!(out, text);
        let facts = facts.expect("facts");
        assert_eq!((facts.reason.as_str(), facts.requested), ("snippet", true));

        settings.self_correction_llm_enabled = false;
        let (_, facts) =
            tauri::async_runtime::block_on(apply_in_language(&settings, text, None, Some("de")));
        let facts = facts.expect("facts");
        assert_eq!(
            (facts.reason.as_str(), facts.requested),
            ("disabled", false)
        );
    }

    #[test]
    fn journal_cue_lists_the_detected_kinds() {
        let mut settings = crate::settings::get_default_settings();
        settings.self_correction_llm_enabled = false;
        let text = "Öhm, das ist ist gut, nein warte, sehr gut.".to_string();
        let (_, facts) =
            tauri::async_runtime::block_on(apply_in_language(&settings, text, None, Some("de")));
        assert_eq!(
            facts.expect("facts").cue,
            "cue:nein warte,filler,repetition"
        );
    }
}
