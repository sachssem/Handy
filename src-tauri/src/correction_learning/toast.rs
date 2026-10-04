//! Learned-correction toast window (fork feature: voice-control).
//!
//! A self-dismissing toast for new suggestions (Accept / Never) and promoted
//! corrections (Undo).
//! It is a **separate** window from the recording overlay ([`crate::overlay`]):
//! the overlay is bound to the record lifecycle and non-interactive, whereas the
//! toast owns its dismissal timer and must be clickable. The two only share the
//! same NSPanel recipe and monitor math.
//!
//! ## Focus policy (macOS)
//!
//! The toast has one hard requirement: it must be clickable (Accept / Never /
//! Undo) yet must never take keyboard focus from the app the user is typing in
//! — not when it appears, and not when a button is clicked. A user pressing
//! Return in a chat composer right after the toast appeared must still send
//! the message. It is therefore the overlay's recipe exactly:
//!
//! - **never key**: `can_become_key_window: false` and `focusable(false)`. The
//!   `NonactivatingPanel` style mask alone only keeps Handy from being
//!   *activated*; a non-activating panel can still become the key window and
//!   then receives every keystroke while the user's app stays frontmost;
//! - **revealed via the panel API** ([`reveal`] → `orderFrontRegardless`), not
//!   [`tauri::WebviewWindow::show`]: tao implements `show` as
//!   `makeKeyAndOrderFront:`, which made the panel key on every reveal;
//! - **clickable without focus**: `accept_first_mouse(true)` makes the WKWebView
//!   take the first click in a window that is not key (wry overrides
//!   `acceptsFirstMouse:`), and the non-activating style mask keeps that click
//!   from activating Handy. Mouse events go to the window under the cursor
//!   regardless of key status — the recording overlay's cancel button works the
//!   same way on the same panel config;
//! - **hidden with `orderOut:`** (`WebviewWindow::hide`), which never activates
//!   anything; as the panel is never key, AppKit has no key window to hand on.
//!
//! The buttons also have keyboard shortcuts ([`super::toast_shortcuts`]),
//! registered only while the toast is visible: [`show_learned_toast`] arms them
//! after a successful reveal, every hide path disarms them.
//!
//! Windows/Linux get a plain always-on-top webview window as a graceful
//! fallback. The learning session that fires the toast only runs on macOS
//! ([`super::session`]), so in practice the toast never shows elsewhere; the
//! non-macOS path exists only to keep the build whole.

use crate::correction_learning::LearnedCorrectionEvent;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Manager, PhysicalPosition, PhysicalSize};

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

#[cfg(target_os = "macos")]
use tauri_nspanel::{tauri_panel, CollectionBehavior, PanelBuilder, PanelLevel, StyleMask};

/// Window label; also used by the frontend `hide_learned_toast` command target.
const TOAST_LABEL: &str = "learned_toast";

/// Bumped on every reveal; the failsafe hide fires only when no newer reveal
/// has restarted the clock.
static SHOW_GENERATION: AtomicU64 = AtomicU64::new(0);
static CURRENT_TOAST: Mutex<Option<RevealIdentity>> = Mutex::new(None);

#[derive(Debug, PartialEq)]
struct RevealIdentity {
    generation: u64,
    id: String,
}

fn hide_matches(
    current: Option<&RevealIdentity>,
    id: Option<&str>,
    generation: Option<u64>,
) -> bool {
    match current {
        Some(current) => {
            id.is_none_or(|id| id == current.id)
                && generation.is_none_or(|generation| generation == current.generation)
        }
        None => id.is_none() && generation.is_none(),
    }
}

/// The webview auto-dismisses after 5–8 s plus its exit animation; this timer
/// leaves slack beyond that and guarantees dismissal if an occluded webview
/// stalls its timers and rAF. Rust orders the window out regardless.
const FAILSAFE_HIDE: Duration = Duration::from_secs(11);

/// After a shortcut acted, the webview's exit animation (200 ms) normally
/// hides the window; this orders it out if the webview stalls.
const SHORTCUT_HIDE_BACKUP: Duration = Duration::from_millis(600);

/// Toast window size (logical points). The pill card is centered inside this
/// frame, with vertical slack for the slide-in/out animation; keep it at least
/// as large as the `.lt-card` footprint in LearnedToast.css.
const TOAST_WIDTH: f64 = 420.0;
const TOAST_HEIGHT: f64 = 76.0;

/// Margin above the screen's bottom edge, clearing the Dock comfortably.
const TOAST_BOTTOM_OFFSET: f64 = 96.0;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(LearnedToastPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

/// Bottom-center position (logical points) on the monitor under the cursor, a
/// comfortable margin above the Dock. Mirrors [`crate::overlay`]'s monitor math
/// but is always bottom-centered — the toast has no top/bottom setting.
fn toast_position(app_handle: &AppHandle, width: f64, height: f64) -> Option<(f64, f64)> {
    let monitor = monitor_with_cursor(app_handle)?;
    let scale = monitor.scale_factor();
    let monitor_x = monitor.position().x as f64 / scale;
    let monitor_y = monitor.position().y as f64 / scale;
    let monitor_width = monitor.size().width as f64 / scale;
    let monitor_height = monitor.size().height as f64 / scale;

    let x = monitor_x + (monitor_width - width) / 2.0;
    let y = monitor_y + monitor_height - height - TOAST_BOTTOM_OFFSET;
    Some((x, y))
}

/// The monitor the cursor is on, falling back to the primary monitor. Kept
/// self-contained (rather than reusing the overlay's private helper) so the
/// feature stays independently droppable.
fn monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some((cursor_x, cursor_y)) = crate::input::get_cursor_position(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                // Normalize to logical coordinates the same way the overlay does
                // (enigo may report logical while Tauri reports physical).
                let scale = monitor.scale_factor();
                let pos = PhysicalPosition::new(
                    (monitor.position().x as f64 / scale) as i32,
                    (monitor.position().y as f64 / scale) as i32,
                );
                let size = PhysicalSize::new(
                    (monitor.size().width as f64 / scale) as u32,
                    (monitor.size().height as f64 / scale) as u32,
                );
                if cursor_x >= pos.x
                    && cursor_x < pos.x + size.width as i32
                    && cursor_y >= pos.y
                    && cursor_y < pos.y + size.height as i32
                {
                    return Some(monitor);
                }
            }
        }
    }

    app_handle.primary_monitor().ok().flatten()
}

/// Creates the learned-correction toast panel and keeps it hidden by default
/// (macOS). See the module docs for the non-activating focus policy.
#[cfg(target_os = "macos")]
pub fn create_learned_toast(app_handle: &AppHandle) {
    let (x, y) = toast_position(app_handle, TOAST_WIDTH, TOAST_HEIGHT).unwrap_or((0.0, 0.0));
    match PanelBuilder::<_, LearnedToastPanel>::new(app_handle, TOAST_LABEL)
        .url(WebviewUrl::App("src/toast/index.html".into()))
        .title("Learned correction")
        .position(tauri::Position::Logical(tauri::LogicalPosition { x, y }))
        .level(PanelLevel::Status)
        .size(tauri::Size::Logical(tauri::LogicalSize {
            width: TOAST_WIDTH,
            height: TOAST_HEIGHT,
        }))
        .has_shadow(false)
        .transparent(true)
        .no_activate(true)
        .corner_radius(0.0)
        .style_mask(StyleMask::empty().borderless().nonactivating_panel())
        // Never focusable (see the module docs), but the first mouse click
        // reaches the buttons; `nonactivating_panel` keeps that click from
        // activating Handy.
        .with_window(|w| {
            w.decorations(false)
                .transparent(true)
                .focusable(false)
                .accept_first_mouse(true)
        })
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .build()
    {
        Ok(panel) => {
            panel.hide();
            watch_toast_window(app_handle);
        }
        Err(e) => {
            log::error!("Failed to create learned-correction toast panel: {}", e);
        }
    }
}

/// Release the toast shortcuts if the window ever goes away for good.
fn watch_toast_window(app_handle: &AppHandle) {
    if let Some(window) = app_handle.get_webview_window(TOAST_LABEL) {
        let app = app_handle.clone();
        window.on_window_event(move |event| {
            if let tauri::WindowEvent::Destroyed = event {
                super::toast_shortcuts::disarm(&app);
            }
        });
    }
}

/// Creates the learned-correction toast window and keeps it hidden by default
/// (non-macOS fallback — never actually shown, see the module docs).
#[cfg(not(target_os = "macos"))]
pub fn create_learned_toast(app_handle: &AppHandle) {
    let mut builder = WebviewWindowBuilder::new(
        app_handle,
        TOAST_LABEL,
        tauri::WebviewUrl::App("src/toast/index.html".into()),
    )
    .title("Learned correction")
    .resizable(false)
    .inner_size(TOAST_WIDTH, TOAST_HEIGHT)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focused(false)
    .visible(false);

    if let Some(data_dir) = crate::portable::data_dir() {
        builder = builder.data_directory(data_dir.join("webview"));
    }

    match builder.build() {
        Ok(_) => watch_toast_window(app_handle),
        Err(e) => log::debug!("Failed to create learned-correction toast window: {}", e),
    }
}

/// The correction awaiting display, set just before [`show_learned_toast`]. The
/// toast window normally exists from startup ([`init_learned_toast`]), but when
/// it ever has to be (re)created on demand, the freshly loaded webview reads
/// this on mount ([`take_pending_learned_toast`]) instead of relying on the
/// `LearnedCorrectionEvent` it missed while loading.
static PENDING: Mutex<Option<LearnedCorrectionEvent>> = Mutex::new(None);

/// Record the correction the toast should show next. Called from the learning
/// session ([`super::session`]) right before [`show_learned_toast`].
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn set_pending_learned_toast(event: LearnedCorrectionEvent) {
    if let Ok(mut pending) = PENDING.lock() {
        *pending = Some(event);
    }
}

/// Take the correction waiting to be shown, if any. The toast webview calls this
/// on mount to render the pair that triggered its (possibly lazy) creation.
#[tauri::command]
#[specta::specta]
pub fn take_pending_learned_toast() -> Option<LearnedCorrectionEvent> {
    PENDING.lock().ok().and_then(|mut pending| pending.take())
}

/// Create the toast window at startup (hidden), mirroring the overlay. A panel
/// webview created lazily — hidden, app inactive — is suspended by WebKit
/// before its first frame commits, which made the toast invisible until the
/// app was reactivated. One created while the app launches keeps rendering for
/// the process lifetime. The lazy creation inside [`show_learned_toast`]
/// remains as a safety net.
pub fn init_learned_toast(app_handle: &AppHandle) {
    // The learning session only exists on macOS; other platforms must not pay
    // an idle webview for a toast that can never fire.
    #[cfg(target_os = "macos")]
    if app_handle.get_webview_window(TOAST_LABEL).is_none() {
        create_learned_toast(app_handle);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app_handle;
}

/// Webview-side lifecycle breadcrumbs. The toast window has no visible dev
/// console, so the component reports its stages (mount, pending taken, shown)
/// into the app log — the only way to see where the display chain stops.
#[tauri::command]
#[specta::specta]
pub fn toast_stage(stage: String) {
    log::debug!("toast-webview: {}", stage);
}

/// Show a sample suggestion toast on the running instance (`handy
/// --debug-toast`, forwarded via single-instance). Exercises the exact
/// production path — stash, lazy window creation, positioning, reveal —
/// without needing a real dictation + manual correction round trip. The ids
/// match no stored pair, so its Undo is a harmless no-op.
pub fn debug_show_learned_toast(app: &AppHandle) {
    use tauri_specta::Event as _;
    log::info!("debug-toast: staging sample suggestion toast");
    let event = LearnedCorrectionEvent {
        id: "debug-toast".to_string(),
        misheard: "raha".to_string(),
        intended: "waha".to_string(),
        status: crate::correction_learning::CorrectionStatus::Suggested,
        suggested_ids: vec!["debug-toast".to_string(), "debug-toast-2".to_string()],
        active_ids: Vec::new(),
        extra: 1,
    };
    set_pending_learned_toast(event.clone());
    // Mirror the real path: the stash feeds a cold first mount, the event
    // refreshes an already-open webview.
    if let Err(err) = event.emit(app) {
        log::error!("Failed to emit debug toast event: {}", err);
    }
    show_learned_toast(app, event);
}

/// Position the toast bottom-center on the active screen and show it without
/// activating Handy. Called from the learning session ([`super::session`]) right
/// after the `LearnedCorrectionEvent` is emitted.
///
/// The window normally exists from startup ([`init_learned_toast`]); creating
/// it here is only the safety net — and why the run is dispatched to the main
/// thread (window creation is main-thread only) and idempotent. Content arrives
/// via the emitted event, or via [`take_pending_learned_toast`] on a cold
/// mount.
///
/// Only the macOS learning session calls this; elsewhere the toast never fires.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn show_learned_toast(app_handle: &AppHandle, event: LearnedCorrectionEvent) {
    let generation = {
        let mut current = CURRENT_TOAST.lock().unwrap_or_else(|e| e.into_inner());
        let generation = SHOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        *current = Some(RevealIdentity {
            generation,
            id: event.id.clone(),
        });
        generation
    };
    let app = app_handle.clone();
    if let Err(e) = app_handle.run_on_main_thread(move || {
        let current = CURRENT_TOAST.lock().unwrap_or_else(|e| e.into_inner());
        if !hide_matches(current.as_ref(), Some(&event.id), Some(generation)) {
            return;
        }
        let existed = app.get_webview_window(TOAST_LABEL).is_some();
        if !existed {
            create_learned_toast(&app);
        }
        match app.get_webview_window(TOAST_LABEL) {
            Some(window) => {
                let position = toast_position(&app, TOAST_WIDTH, TOAST_HEIGHT);
                if let Some((x, y)) = position {
                    let _ = window
                        .set_position(tauri::Position::Logical(tauri::LogicalPosition { x, y }));
                }
                let show_result = reveal(&app, &window);
                log::debug!(
                    "toast-show: existed={}, position={:?}, show={:?}, visible_after={:?}",
                    existed,
                    position,
                    show_result,
                    window.is_visible()
                );
                if show_result.is_ok() {
                    super::toast_shortcuts::arm(&app, generation, &event);
                }
            }
            None => {
                log::error!("toast-show: window missing after creation attempt");
            }
        }
    }) {
        log::error!("Failed to show learned-correction toast: {}", e);
        return;
    }

    // Failsafe hide, independent of the webview's own timers.
    hide_if_current(app_handle, generation, FAILSAFE_HIDE, "failsafe");
}

/// Order the toast front without making it key (see the module docs). Never
/// falls back to `WebviewWindow::show`, which would make it key.
#[cfg(target_os = "macos")]
fn reveal(app: &AppHandle, _window: &tauri::WebviewWindow) -> Result<(), String> {
    use tauri_nspanel::ManagerExt;
    let panel = app
        .get_webview_panel(TOAST_LABEL)
        .map_err(|e| format!("toast panel not registered: {:?}", e))?;
    panel.show();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn reveal(_app: &AppHandle, window: &tauri::WebviewWindow) -> Result<(), String> {
    window.show().map_err(|e| e.to_string())
}

/// After `delay`, order the toast out (and release its shortcuts) unless a
/// newer reveal has taken over in the meantime.
fn hide_if_current(app_handle: &AppHandle, generation: u64, delay: Duration, reason: &'static str) {
    let app = app_handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        if SHOW_GENERATION.load(Ordering::SeqCst) != generation {
            return; // a newer reveal restarted the clock
        }
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            let mut current = CURRENT_TOAST.lock().unwrap_or_else(|e| e.into_inner());
            if !hide_matches(current.as_ref(), None, Some(generation)) {
                return;
            }
            *current = None;
            if let Some(window) = app_main.get_webview_window(TOAST_LABEL) {
                if window.is_visible().unwrap_or(false) {
                    log::debug!("toast-hide: {}", reason);
                    let _ = window.hide();
                }
            }
            super::toast_shortcuts::disarm(&app_main);
        });
    });
}

/// Event telling the toast webview a keyboard shortcut acted on it; the payload
/// is the toast's first-pair id, so a dismissal racing a newer toast is ignored.
const SHORTCUT_DISMISS_EVENT: &str = "learned-toast-shortcut-dismiss";

/// A toast shortcut already performed its action: let the webview play its
/// exit animation, and order the window out shortly after in case the webview
/// is suspended (the same stall the failsafe exists for).
pub(super) fn dismiss_after_shortcut(app: &AppHandle, generation: u64, toast_id: &str) {
    use tauri::Emitter;
    if let Err(e) = app.emit(SHORTCUT_DISMISS_EVENT, toast_id) {
        log::error!("Failed to emit toast dismiss event: {}", e);
    }
    hide_if_current(app, generation, SHORTCUT_HIDE_BACKUP, "shortcut-backup");
}

/// Hide the toast window after auto-dismissal or a user action
/// (the exit animation plays first, then this orders the window
/// out so it stops intercepting clicks in the bottom-center region).
#[tauri::command]
#[specta::specta]
pub fn hide_learned_toast(app: AppHandle, id: Option<String>) -> Result<(), String> {
    let app_main = app.clone();
    app.run_on_main_thread(move || {
        let mut current = CURRENT_TOAST.lock().unwrap_or_else(|e| e.into_inner());
        if !hide_matches(current.as_ref(), id.as_deref(), None) {
            return;
        }
        *current = None;
        log::debug!("toast-hide: webview dismiss");
        super::toast_shortcuts::disarm(&app_main);
        if let Some(window) = app_main.get_webview_window(TOAST_LABEL) {
            if let Err(e) = window.hide() {
                log::warn!("Failed to hide learned-correction toast: {}", e);
            }
        }
    })
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hides_are_scoped_to_the_current_toast_id_and_generation() {
        let current = RevealIdentity {
            generation: 2,
            id: "new".into(),
        };
        assert!(!hide_matches(Some(&current), Some("old"), None));
        assert!(!hide_matches(Some(&current), None, Some(1)));
        assert!(hide_matches(Some(&current), Some("new"), Some(2)));
        assert!(hide_matches(Some(&current), None, None));
        assert!(!hide_matches(None, Some("old"), None));
        assert!(!hide_matches(None, None, Some(2)));
        assert!(hide_matches(None, None, None));
    }
}
