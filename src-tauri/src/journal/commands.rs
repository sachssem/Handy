//! Tauri commands for the dictation journal (fork feature: voice-control).

use crate::settings;
use tauri::AppHandle;
use tauri_plugin_opener::OpenerExt;

/// Master switch for writing the journal.
#[tauri::command]
#[specta::specta]
pub fn change_dictation_journal_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.dictation_journal_enabled = enabled;
    settings::write_settings(&app, settings);
    super::set_enabled(enabled);
    Ok(())
}

/// Days a journal day file is kept (clamped to 1–365); prunes immediately.
#[tauri::command]
#[specta::specta]
pub fn change_dictation_journal_retention_days_setting(
    app: AppHandle,
    days: u32,
) -> Result<(), String> {
    let days = super::writer::clamp_retention(days);
    let mut settings = settings::get_settings(&app);
    settings.dictation_journal_retention_days = days;
    settings::write_settings(&app, settings);
    super::set_retention_days(days);
    Ok(())
}

/// Absolute path of the journal directory (for agents / diagnostics).
#[tauri::command]
#[specta::specta]
pub fn get_journal_dir_path(app: AppHandle) -> Result<String, String> {
    super::journal_dir(&app)
        .map(|dir| dir.to_string_lossy().to_string())
        .ok_or_else(|| "Failed to resolve the journal directory".to_string())
}

/// Reveal the journal directory in the file manager (created if missing).
#[tauri::command]
#[specta::specta]
pub fn open_journal_dir(app: AppHandle) -> Result<(), String> {
    let dir = super::journal_dir(&app)
        .ok_or_else(|| "Failed to resolve the journal directory".to_string())?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create the journal directory: {}", e))?;
    app.opener()
        .open_path(dir.to_string_lossy().to_string(), None::<String>)
        .map_err(|e| format!("Failed to open the journal directory: {}", e))
}

/// Recording-overlay webview breadcrumbs (`overlay: show '<state>' handler|
/// first-frame epoch_ms=N …`, see `RecordingOverlay.tsx`). The overlay has no
/// visible console, so its show latencies reach the app log (debug level, the
/// `toast-webview:` prefix kept for log analyses predating this command) and
/// the journal's `overlay` events through here.
#[tauri::command]
#[specta::specta]
pub fn journal_overlay_stage(stage: String) {
    log::debug!("toast-webview: {}", stage);
    super::dictation::record_overlay_breadcrumb(&stage);
}
