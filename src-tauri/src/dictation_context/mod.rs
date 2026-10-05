//! App context captured at dictation start (fork feature: voice-control).
//!
//! When a recording starts, [`capture_async`] records which app and field the
//! user is dictating into — bundle id, app name, window title, focused element
//! role, and up to [`MAX_CONTEXT_CHARS`] characters before the caret — on a
//! background thread, so neither the overlay nor the microphone start waits
//! for it. The result is kept per dictation id ([`get`]) for later pipeline
//! stages (context-aware ASR prompts, LLM styling) and written to the
//! dictation journal.
//!
//! - Reuses `correction_learning::ax_reader` for every AX call; each AX
//!   message is capped at [`AX_BUDGET`].
//! - While secure event input is on, only the app identity is captured (no
//!   focused-element read at all). A secure (password) field's text is never
//!   read and its window title is dropped. Either way the dictation's text is
//!   redacted from the journal.
//! - macOS only; elsewhere the capture is a no-op.

use crate::correction_learning::ax_reader;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tauri::AppHandle;

/// Characters of text before the caret kept per dictation.
pub const MAX_CONTEXT_CHARS: usize = 500;
/// Per-message AX timeout and soft budget of one capture.
const AX_BUDGET: Duration = Duration::from_millis(150);
/// Contexts kept in memory (the most recent dictations).
const KEEP: usize = 8;

/// The context of one dictation. Serialized as the journal's `context` event.
#[derive(Debug, Clone, Default, Serialize)]
pub struct DictationContext {
    /// Dictation id (see `journal`).
    pub id: u64,
    /// Epoch ms when the capture started.
    pub captured_at_ms: u64,
    /// Capture wall time, ms (main-thread hop excluded).
    pub capture_ms: u64,
    pub bundle_id: Option<String>,
    pub app_name: Option<String>,
    pub window_title: Option<String>,
    /// AX role of the focused element (`AXTextArea`, `AXTextField`, …).
    pub focused_role: Option<String>,
    pub focused_subrole: Option<String>,
    /// The focused element is a secure (password) field; no text was read.
    pub secure: bool,
    /// Secure event input was on at capture; no focused element was read.
    pub secure_input: bool,
    /// Up to [`MAX_CONTEXT_CHARS`] characters immediately before the caret.
    pub text_before_caret: Option<String>,
    /// Selected length in UTF-16 units (0 = plain caret).
    pub selection_len: Option<usize>,
    /// The caret was read at the very start of the field (nothing before it,
    /// an empty field included). Omitted from the journal when false.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub caret_at_start: bool,
    /// `string_for_range` | `value` — how the text was read.
    pub text_source: Option<String>,
    /// Why the capture stopped early, if it did.
    pub error: Option<String>,
    /// Set on the journal copy only: why the window title and the text
    /// before the caret were dropped (the dictation's redaction reason).
    pub text_redacted: Option<String>,
}

static STORE: Mutex<VecDeque<Arc<DictationContext>>> = Mutex::new(VecDeque::new());

fn store(context: Arc<DictationContext>) {
    let mut contexts = STORE.lock().unwrap_or_else(|e| e.into_inner());
    contexts.retain(|c| c.id != context.id);
    contexts.push_back(context);
    while contexts.len() > KEEP {
        contexts.pop_front();
    }
}

/// The context captured for dictation `id`, once its capture finished.
pub fn get(id: u64) -> Option<Arc<DictationContext>> {
    STORE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .rev()
        .find(|c| c.id == id)
        .cloned()
}

/// Capture the frontmost app's context for dictation `id` without blocking
/// the caller: the frontmost pid is resolved on the main thread (where
/// `NSWorkspace` is current), the AX reads run on a fresh thread.
#[cfg(target_os = "macos")]
pub fn capture_async(app: &AppHandle, id: u64) {
    let captured_at_ms = crate::journal::now_ms();
    let dispatched = app.run_on_main_thread(move || {
        let pid = ax_reader::frontmost_pid();
        let spawned = std::thread::Builder::new()
            .name("dictation-context".into())
            .spawn(move || {
                let context = Arc::new(capture(id, pid, captured_at_ms));
                log::debug!(
                    "context: dictation {} — app {}, role {}, {} chars before caret, {} ms{}",
                    id,
                    context.bundle_id.as_deref().unwrap_or("?"),
                    context.focused_role.as_deref().unwrap_or("?"),
                    context
                        .text_before_caret
                        .as_deref()
                        .map_or(0, |t| t.chars().count()),
                    context.capture_ms,
                    context
                        .error
                        .as_deref()
                        .map(|e| format!(" ({e})"))
                        .unwrap_or_default()
                );
                store(Arc::clone(&context));
                crate::journal::record_context(&context);
            });
        if let Err(err) = spawned {
            log::debug!("context: capture thread failed to start: {}", err);
        }
    });
    if let Err(err) = dispatched {
        log::debug!("context: main-thread dispatch failed: {}", err);
    }
}

/// No Accessibility API: nothing to capture.
#[cfg(not(target_os = "macos"))]
pub fn capture_async(_app: &AppHandle, _id: u64) {}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn capture(id: u64, pid: Option<i32>, captured_at_ms: u64) -> DictationContext {
    let started = Instant::now();
    let mut context = DictationContext {
        id,
        captured_at_ms,
        ..Default::default()
    };
    let Some(pid) = pid else {
        context.error = Some("no_frontmost_app".into());
        return context;
    };
    let (bundle_id, app_name) = ax_reader::app_identity(pid);
    context.bundle_id = bundle_id;
    context.app_name = app_name;

    // Secure event input (a password prompt anywhere, a terminal's secure
    // keyboard entry): read neither the window title nor the field.
    if crate::secure_input::is_enabled_now() {
        context.secure_input = true;
        context.error = Some("secure_input".into());
        context.capture_ms = started.elapsed().as_millis() as u64;
        return context;
    }

    // UTF-16 budget: two units per char covers astral characters.
    let read = ax_reader::read_focus_context(pid, MAX_CONTEXT_CHARS * 2, AX_BUDGET);
    context.focused_role = read.role;
    context.focused_subrole = read.subrole;
    context.secure = read.secure;
    context.selection_len = read.selection_len;
    context.caret_at_start = read.caret_at_start && !read.secure;
    context.error = read.error.map(str::to_string);
    if !read.secure {
        // A password dialog's window title can name the account; drop it.
        context.window_title = read.window_title;
        if let Some(units) = read.text_utf16 {
            let text = text_before_caret(&units, read.caret_utf16, MAX_CONTEXT_CHARS);
            if !text.is_empty() {
                context.text_before_caret = Some(text);
                context.text_source = read.text_source.map(str::to_string);
            }
        }
    }
    context.capture_ms = started.elapsed().as_millis() as u64;
    context
}

/// The last `max_chars` characters before `caret_utf16` in `units`. The caret
/// is clamped to the text; a surrogate pair split by the slice start (or the
/// caret) is dropped rather than turned into a replacement character.
pub(crate) fn text_before_caret(units: &[u16], caret_utf16: usize, max_chars: usize) -> String {
    let before = &units[..caret_utf16.min(units.len())];
    let decoded: String = char::decode_utf16(before.iter().copied())
        .filter_map(Result::ok)
        .collect();
    let skip = decoded.chars().count().saturating_sub(max_chars);
    decoded.chars().skip(skip).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn slices_before_the_caret_on_char_boundaries() {
        let units = utf16("Grüße aus Köln");
        // Caret after "Grüße" (5 chars, 5 UTF-16 units — umlauts are one unit).
        assert_eq!(text_before_caret(&units, 5, 500), "Grüße");
        assert_eq!(text_before_caret(&units, units.len(), 4), "Köln");
        assert_eq!(text_before_caret(&units, 0, 500), "");
    }

    #[test]
    fn caret_past_the_end_is_clamped() {
        let units = utf16("Hallo");
        assert_eq!(text_before_caret(&units, 999, 500), "Hallo");
    }

    #[test]
    fn caps_to_the_last_chars_without_splitting_multibyte_chars() {
        let text = format!("{}äöü", "a".repeat(600));
        let units = utf16(&text);
        let out = text_before_caret(&units, units.len(), 500);
        assert_eq!(out.chars().count(), 500);
        assert!(out.ends_with("äöü"));
    }

    #[test]
    fn split_surrogate_pairs_are_dropped_not_mangled() {
        let units = utf16("😀x😀");
        // Slice starting on the low half of the first emoji.
        assert_eq!(text_before_caret(&units[1..], units.len() - 1, 10), "x😀");
        // Caret between the halves of the last emoji.
        assert_eq!(text_before_caret(&units, 4, 10), "😀x");
        // A cap counts chars, not units.
        assert_eq!(text_before_caret(&units, units.len(), 1), "😀");
    }

    #[test]
    fn contexts_are_kept_per_id_and_bounded() {
        for id in 1..=(KEEP as u64 + 3) {
            store(Arc::new(DictationContext {
                id: 9_000_000 + id,
                ..Default::default()
            }));
        }
        assert!(get(9_000_001).is_none(), "oldest evicted");
        assert!(get(9_000_000 + KEEP as u64 + 3).is_some());
    }
}
