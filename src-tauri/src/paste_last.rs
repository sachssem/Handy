//! "Paste last transcript" hotkey: re-pastes the most recent dictation's final
//! text into the focused app through the same paste path a dictation uses
//! (paste method, delays, clipboard restore, trailing space, auto-submit), so
//! the result is identical to the original paste.
//!
//! A re-paste is not a dictation: it opens no journal record and no
//! correction-learning session, and it only fires while Handy is idle.

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use log::{debug, error, info};
use tauri::{AppHandle, Emitter, Manager};

use crate::actions::ShortcutAction;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::history::{HistoryEntry, HistoryManager};
use crate::settings::ShortcutBinding;
use crate::tray::TrayIconState;

pub const BINDING_ID: &str = "paste_last_transcript";
const REPASTE_COOLDOWN: Duration = Duration::from_millis(600);

#[derive(Default)]
struct RepasteDedupe {
    pressed: bool,
    last_repaste: Option<Instant>,
}

impl RepasteDedupe {
    fn press(&mut self, now: Instant) -> bool {
        if self.pressed {
            return false;
        }
        self.pressed = true;
        if self
            .last_repaste
            .is_some_and(|last| now.saturating_duration_since(last) < REPASTE_COOLDOWN)
        {
            return false;
        }
        self.last_repaste = Some(now);
        true
    }

    fn release(&mut self) {
        self.pressed = false;
    }
}

static REPASTE_DEDUPE: Mutex<RepasteDedupe> = Mutex::new(RepasteDedupe {
    pressed: false,
    last_repaste: None,
});

/// Default binding. Ctrl+V on macOS, where it is free (paste is Cmd+V) and
/// sits next to the Ctrl+X dictation key. On Windows/Linux Ctrl+V *is* paste —
/// and Handy's own Ctrl+V paste method would re-trigger the hotkey — so the
/// default there stays off the system paste combo.
pub fn default_binding() -> ShortcutBinding {
    #[cfg(target_os = "macos")]
    let default = "ctrl+v";
    #[cfg(not(target_os = "macos"))]
    let default = "alt+shift+v";

    ShortcutBinding {
        id: BINDING_ID.to_string(),
        name: "Paste Last Transcript".to_string(),
        description: "Pastes your most recent transcription again.".to_string(),
        default_binding: default.to_string(),
        current_binding: default.to_string(),
    }
}

pub struct PasteLastTranscriptAction;

impl ShortcutAction for PasteLastTranscriptAction {
    fn start(&self, app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        let should_paste = match REPASTE_DEDUPE.lock() {
            Ok(mut dedupe) => dedupe.press(Instant::now()),
            Err(err) => {
                error!("paste-last: failed to lock hotkey dedupe: {}", err);
                return;
            }
        };
        if !should_paste {
            return;
        }
        paste_last_transcript(app);
    }

    fn stop(&self, _app: &AppHandle, _binding_id: &str, _shortcut_str: &str) {
        match REPASTE_DEDUPE.lock() {
            Ok(mut dedupe) => dedupe.release(),
            Err(err) => error!("paste-last: failed to release hotkey dedupe: {}", err),
        }
    }
}

/// The text a re-paste uses: the final pasted text (the fork stores it in
/// `post_processed_text` when it differs from the raw transcription), else the
/// raw transcription — the same selection as the tray's "Copy last transcript".
/// `None` when there is nothing worth pasting.
fn repaste_text(entry: Option<HistoryEntry>) -> Option<String> {
    let entry = entry?;
    let text = crate::tray::last_transcript_text(&entry);
    (!text.trim().is_empty()).then(|| text.to_string())
}

/// A dictation is recording or still processing. The tray state covers
/// recording→paste; `is_recording` covers the press before the tray flips.
fn is_busy(app: &AppHandle) -> bool {
    let recording = app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|audio| audio.is_recording());
    recording || crate::tray::current_tray_state(app) != TrayIconState::Idle
}

fn paste_last_transcript(app: &AppHandle) {
    if is_busy(app) {
        debug!("paste-last: ignored — a dictation is in progress");
        return;
    }

    let history = app.state::<Arc<HistoryManager>>();
    let entry = match history.get_latest_completed_entry() {
        Ok(entry) => entry,
        Err(err) => {
            error!("paste-last: failed to read the last transcript: {}", err);
            return;
        }
    };
    let Some(text) = repaste_text(entry) else {
        debug!("paste-last: no transcript to paste yet");
        return;
    };

    // The previous dictation's learning session must not read the re-pasted
    // text as a user edit; end it before the paste lands.
    crate::correction_learning::end_session();

    let chars = text.chars().count();
    let ah = app.clone();
    if let Err(err) = app.run_on_main_thread(move || match crate::utils::paste(text, ah.clone()) {
        Ok(()) => info!("paste-last: re-pasted last transcript ({} chars)", chars),
        Err(e) => {
            error!("paste-last: failed to paste: {}", e);
            let _ = ah.emit("paste-error", ());
        }
    }) {
        error!("paste-last: failed to run paste on main thread: {:?}", err);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actions::ACTION_MAP;
    use crate::settings::get_default_settings;

    #[test]
    fn held_binding_ignores_auto_repeat_even_after_cooldown() {
        let mut dedupe = RepasteDedupe::default();
        let now = Instant::now();
        assert!(dedupe.press(now));
        assert!(!dedupe.press(now + Duration::from_millis(10)));
        assert!(!dedupe.press(now + Duration::from_secs(2)));
        dedupe.release();
        assert!(dedupe.press(now + Duration::from_secs(3)));
    }

    #[test]
    fn released_binding_must_wait_six_hundred_ms_between_repastes() {
        let mut dedupe = RepasteDedupe::default();
        let now = Instant::now();
        assert!(dedupe.press(now));
        dedupe.release();
        assert!(!dedupe.press(now + Duration::from_millis(599)));
        dedupe.release();
        assert!(dedupe.press(now + Duration::from_millis(600)));
    }

    #[test]
    fn press_during_cooldown_stays_blocked_until_release() {
        let mut dedupe = RepasteDedupe::default();
        let now = Instant::now();
        assert!(dedupe.press(now));
        dedupe.release();
        assert!(!dedupe.press(now + Duration::from_millis(100)));
        assert!(!dedupe.press(now + Duration::from_secs(1)));
        dedupe.release();
        assert!(dedupe.press(now + Duration::from_secs(1)));
    }

    fn entry(transcription: &str, post_processed: Option<&str>) -> HistoryEntry {
        HistoryEntry {
            id: 1,
            file_name: "handy-1.wav".to_string(),
            timestamp: 0,
            saved: false,
            title: "Recording".to_string(),
            transcription_text: transcription.to_string(),
            post_processed_text: post_processed.map(str::to_string),
            post_process_prompt: None,
            post_process_requested: false,
        }
    }

    #[test]
    fn prefers_the_final_pasted_text() {
        let text = repaste_text(Some(entry("raw text", Some("Final text."))));
        assert_eq!(text.as_deref(), Some("Final text."));
    }

    #[test]
    fn falls_back_to_the_raw_transcription() {
        let text = repaste_text(Some(entry("raw text", None)));
        assert_eq!(text.as_deref(), Some("raw text"));
    }

    #[test]
    fn pastes_the_stored_text_verbatim() {
        let text = repaste_text(Some(entry("raw", Some("  spaced  "))));
        assert_eq!(text.as_deref(), Some("  spaced  "));
    }

    #[test]
    fn nothing_to_paste_without_history() {
        assert_eq!(repaste_text(None), None);
    }

    #[test]
    fn nothing_to_paste_for_a_blank_entry() {
        assert_eq!(repaste_text(Some(entry("   ", None))), None);
    }

    /// The settings load path merges every missing default binding into an
    /// existing store, so being in the defaults is what migrates old stores.
    #[test]
    fn binding_is_a_default_binding() {
        let bindings = get_default_settings().bindings;
        let binding = bindings.get(BINDING_ID).expect("default binding");
        assert_eq!(binding.id, BINDING_ID);
        assert_eq!(binding.current_binding, binding.default_binding);
        #[cfg(target_os = "macos")]
        assert_eq!(binding.default_binding, "ctrl+v");
    }

    #[test]
    fn every_default_binding_has_an_action() {
        for id in get_default_settings().bindings.keys() {
            assert!(ACTION_MAP.contains_key(id), "no action for binding '{id}'");
        }
    }
}
