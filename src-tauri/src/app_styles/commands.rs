//! Tauri commands for per-app styles (fork feature: voice-control).

use super::AppStyleCategories;
use crate::settings;
use tauri::AppHandle;

/// Master switch for per-app styles and context matching.
#[tauri::command]
#[specta::specta]
pub fn change_app_styles_enabled_setting(app: AppHandle, enabled: bool) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.app_styles_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}

/// The per-category switches.
#[tauri::command]
#[specta::specta]
pub fn change_app_styles_categories_setting(
    app: AppHandle,
    categories: AppStyleCategories,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.app_styles_categories = categories;
    settings::write_settings(&app, settings);
    Ok(())
}
