//! Tauri command for the self-correction pass (fork feature: voice-control).

use crate::settings;
use tauri::AppHandle;

/// Switch for the trigger-gated self-correction LLM pass.
#[tauri::command]
#[specta::specta]
pub fn change_self_correction_llm_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.self_correction_llm_enabled = enabled;
    settings::write_settings(&app, settings);
    Ok(())
}
