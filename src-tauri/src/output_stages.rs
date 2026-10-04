//! Fork text stages after the transcription result (fork feature:
//! voice-control): the trigger-gated self-correction LLM pass, then the
//! per-app style — the last stage before the paste.
//!
//! Hooked into `TranscribeAction::stop` (one wrapped call around upstream's
//! `process_transcription_output`), so it runs only for live dictations: a
//! history retry stores no final text and has no target app.

use crate::actions::ProcessedTranscription;
use crate::journal::{record_self_correction, record_text_stage, Stage};
use std::future::Future;
use std::ops::Range;
use tauri::AppHandle;

/// Await upstream's output handling, then run the fork's output stages on its
/// final text.
pub(crate) async fn finish(
    app: &AppHandle,
    processed: impl Future<Output = ProcessedTranscription>,
) -> ProcessedTranscription {
    let dictation_id = crate::journal::last_dictation_id();
    let mut processed = processed.await;
    let protection = crate::snippets::take_protection(dictation_id);
    if processed.final_text.trim().is_empty() {
        return processed;
    }
    let settings = crate::settings::get_settings(app);
    let text = std::mem::take(&mut processed.final_text);

    // Snippet expansions (inserted upstream of the text rules) stay verbatim.
    let protected = protection.spans_in(&text);
    let skip = correction_skip(protection.fired, processed.post_processed_text.is_some());
    let (text, facts) = crate::self_correction::apply(&settings, text, skip).await;
    if let Some(facts) = facts {
        if facts.applied {
            record_text_stage(Stage::SelfCorrection, &text);
        }
        record_self_correction(facts);
    }

    processed.final_text = if settings.app_styles_enabled {
        let styled = style_for_target(&settings, &text, protected, dictation_id);
        record_text_stage(Stage::AppStyle, &styled);
        styled
    } else {
        text
    };
    processed
}

fn correction_skip(snippet_fired: bool, post_processed: bool) -> Option<&'static str> {
    if snippet_fired {
        Some("snippet")
    } else if post_processed {
        Some("post_process_applied")
    } else {
        None
    }
}

/// Apply the per-app style for this dictation's target (captured at start).
fn style_for_target(
    settings: &crate::settings::AppSettings,
    text: &str,
    protected: &[Range<usize>],
    dictation_id: Option<u64>,
) -> String {
    let Some(context) = dictation_id.and_then(crate::dictation_context::get) else {
        return text.to_string();
    };
    let lang = crate::correction_learning::last_transcription_language();
    let mut dictionary = settings.custom_words.clone();
    dictionary.extend(
        crate::correction_learning::snapshot()
            .corrections
            .iter()
            .filter(|c| c.is_applied())
            .map(|c| c.intended.clone()),
    );
    style_output(
        settings,
        text,
        crate::app_styles::Target {
            bundle_id: context.bundle_id.as_deref(),
            text_before_caret: context.text_before_caret.as_deref(),
        },
        context.secure,
        lang.as_deref(),
        &dictionary,
        protected,
    )
}

/// Pure final-stage wiring; no settings store, AX capture or provider calls.
fn style_output(
    settings: &crate::settings::AppSettings,
    text: &str,
    target: crate::app_styles::Target,
    secure: bool,
    lang: Option<&str>,
    dictionary: &[String],
    protected: &[Range<usize>],
) -> String {
    if !settings.app_styles_enabled || secure {
        return text.to_string();
    }
    crate::app_styles::style(
        text,
        target,
        &settings.app_styles_categories,
        lang,
        dictionary,
        protected,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_correction_skip_reasons_use_fired_state() {
        assert_eq!(correction_skip(false, false), None);
        assert_eq!(correction_skip(true, false), Some("snippet"));
        assert_eq!(correction_skip(true, true), Some("snippet"));
        assert_eq!(correction_skip(false, true), Some("post_process_applied"));
    }

    #[test]
    fn style_wiring_respects_actual_snippet_spans() {
        let mut settings = crate::settings::get_default_settings();
        settings.app_styles_enabled = true;
        let target = crate::app_styles::Target {
            bundle_id: Some("com.tinyspeck.slackmacgap"),
            text_before_caret: Some("and "),
        };
        assert_eq!(
            style_output(
                &settings,
                "The rest.",
                target,
                false,
                Some("en"),
                &[],
                std::slice::from_ref(&(0..9))
            ),
            "The rest."
        );
        assert_eq!(
            style_output(&settings, "The rest.", target, false, Some("en"), &[], &[]),
            "the rest"
        );
        assert_eq!(
            style_output(&settings, "The rest.", target, true, Some("en"), &[], &[]),
            "The rest."
        );
        settings.app_styles_enabled = false;
        assert_eq!(
            style_output(&settings, "The rest.", target, false, Some("en"), &[], &[]),
            "The rest."
        );
    }

    #[test]
    fn style_wiring_passes_categories_language_and_dictionary() {
        let mut settings = crate::settings::get_default_settings();
        settings.app_styles_enabled = true;
        let target = crate::app_styles::Target {
            bundle_id: Some("com.tinyspeck.slackmacgap"),
            text_before_caret: Some("and "),
        };
        settings.app_styles_categories.chat = false;
        assert_eq!(
            style_output(&settings, "The rest.", target, false, Some("en"), &[], &[]),
            "the rest."
        );
        assert_eq!(
            style_output(&settings, "The rest.", target, false, Some("de"), &[], &[]),
            "The rest."
        );
        assert_eq!(
            style_output(
                &settings,
                "The rest.",
                target,
                false,
                Some("en"),
                &["The".into()],
                &[]
            ),
            "The rest."
        );
    }
}
