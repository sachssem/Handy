//! Learned-correction toast window (fork feature: voice-control).
//!
//! Phase C's user-visible surface: a small, self-dismissing toast shown after a
//! correction is auto-learned, carrying the learned pair and an "Undo" button.
//! It is a **separate** window from the recording overlay ([`crate::overlay`]):
//! the overlay is bound to the record lifecycle and non-interactive, whereas the
//! toast lives ~5 s on its own and must be clickable. The two only share the
//! same NSPanel recipe and monitor math.
//!
//! ## Focus policy (macOS)
//!
//! The toast has one hard requirement: it must be clickable (Undo) yet must
//! never steal focus from the app the user is typing in — not when it appears,
//! and not when Undo is clicked. It is therefore a **non-activating** NSPanel,
//! the same class the overlay uses, but with one flag flipped:
//!
//! - showing it uses [`tauri::WebviewWindow::show`], which on the converted
//!   panel orders the window front without making it key, so appearing never
//!   activates Handy or pulls the caret out of the user's field;
//! - the `NonactivatingPanel` style mask means clicking anywhere in the panel —
//!   including the Undo button — routes the click *without* activating Handy, so
//!   the user's frontmost app stays frontmost;
//! - `can_become_key_window: true` (the overlay uses `false`) lets the webview
//!   become first responder for that click. A non-activating panel can be the
//!   key window without activating its owning app, which is exactly what makes
//!   the Undo button reliably clickable while focus stays put.
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

/// The webview owns the pretty 5s auto-dismiss; this is the guarantee behind
/// it. A lazily created panel's webview can stall its timers (and rAF) while
/// the compositor still considers it occluded, which left the toast stuck on
/// screen — Rust orders it out regardless.
const FAILSAFE_HIDE: Duration = Duration::from_secs(8);

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
            can_become_key_window: true,
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
        // Unlike the overlay we keep the window focusable and accept the first
        // mouse click, so the Undo button responds without a preceding focus
        // click. `nonactivating_panel` keeps that click from activating Handy.
        .with_window(|w| {
            w.decorations(false)
                .transparent(true)
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
        }
        Err(e) => {
            log::error!("Failed to create learned-correction toast panel: {}", e);
        }
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

    if let Err(e) = builder.build() {
        log::debug!("Failed to create learned-correction toast window: {}", e);
    }
}

/// The correction awaiting display, set just before [`show_learned_toast`]. The
/// toast webview is created lazily on the first learned correction, so the
/// `LearnedCorrectionEvent` emitted alongside can reach a listener that has not
/// mounted yet; the freshly loaded webview reads this on mount instead of
/// relying on that event ([`take_pending_learned_toast`]).
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
    if app_handle.get_webview_window(TOAST_LABEL).is_none() {
        create_learned_toast(app_handle);
    }
}

/// Webview-side lifecycle breadcrumbs. The toast window has no visible dev
/// console, so the component reports its stages (mount, pending taken, shown)
/// into the app log — the only way to see where the display chain stops.
#[tauri::command]
#[specta::specta]
pub fn toast_stage(stage: String) {
    log::info!("toast-webview: {}", stage);
}

/// Show a sample trial toast on the running instance (`handy --debug-toast`,
/// forwarded via single-instance). Exercises the exact production path —
/// stash, lazy window creation, positioning, reveal — without needing a real
/// dictation + manual correction round trip.
pub fn debug_show_learned_toast(app: &AppHandle) {
    use tauri_specta::Event as _;
    log::info!("debug-toast: staging sample trial toast");
    let event = LearnedCorrectionEvent {
        id: "debug-toast".to_string(),
        misheard: "raha".to_string(),
        intended: "waha".to_string(),
        trial: true,
        extra: 1,
    };
    set_pending_learned_toast(event.clone());
    // Mirror the real trial path: the stash feeds a cold first mount, the event
    // refreshes an already-open webview.
    if let Err(err) = event.emit(app) {
        log::error!("Failed to emit debug toast event: {}", err);
    }
    show_learned_toast(app);
}

/// Position the toast bottom-center on the active screen and show it without
/// activating Handy. Called from the learning session ([`super::session`]) right
/// after the `LearnedCorrectionEvent` is emitted.
///
/// The webview is created lazily on first use — the feature is default-off, so
/// most installs never build it — which is why the run is dispatched to the main
/// thread (window creation is main-thread only) and idempotent. Its content
/// arrives via the emitted event when it is already open, or via
/// [`take_pending_learned_toast`] on the cold first mount.
///
/// Only the macOS learning session calls this; elsewhere the toast never fires.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn show_learned_toast(app_handle: &AppHandle) {
    let app = app_handle.clone();
    if let Err(e) = app_handle.run_on_main_thread(move || {
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
                let show_result = window.show();
                // Breadcrumbs for the invisible-toast hunt: every value here has
                // at some point been the missing link.
                log::info!(
                    "toast-show: existed={}, position={:?}, show={:?}, visible_after={:?}",
                    existed,
                    position,
                    show_result.as_ref().map(|_| ()),
                    window.is_visible()
                );
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
    let generation = SHOW_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let app = app_handle.clone();
    std::thread::spawn(move || {
        std::thread::sleep(FAILSAFE_HIDE);
        if SHOW_GENERATION.load(Ordering::SeqCst) != generation {
            return; // a newer reveal restarted the clock
        }
        let app_main = app.clone();
        let _ = app.run_on_main_thread(move || {
            if let Some(window) = app_main.get_webview_window(TOAST_LABEL) {
                if window.is_visible().unwrap_or(false) {
                    log::info!("toast-hide: failsafe");
                    let _ = window.hide();
                }
            }
        });
    });
}

/// Hide the toast window. Invoked by the frontend after the 5 s auto-hide timer
/// or an Undo click (the exit animation plays first, then this orders the window
/// out so it stops intercepting clicks in the bottom-center region).
#[tauri::command]
#[specta::specta]
pub fn hide_learned_toast(app: AppHandle) -> Result<(), String> {
    log::info!("toast-hide: webview dismiss");
    if let Some(window) = app.get_webview_window(TOAST_LABEL) {
        window.hide().map_err(|e| e.to_string())?;
    }
    Ok(())
}
