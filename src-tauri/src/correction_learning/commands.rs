//! Tauri commands for the learned-corrections review UI and toast (fork
//! feature: voice-control).
//!
//! Living in the fork-owned module (not `shortcut/mod.rs`) keeps the upstream
//! footprint to the `collect_commands!` registrations in `lib.rs`. Every list
//! mutation goes through [`store::update`], which is atomic against concurrent
//! auto-learning and emits `LearnedCorrectionsChanged`.

use super::differ::Aggressiveness;
use super::store::{self, LearnedCorrection, LearnedCorrections};
use crate::settings;
use log::warn;
use tauri::AppHandle;

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// Master switch for learning *and* applying learned corrections.
#[tauri::command]
#[specta::specta]
pub fn change_learn_corrections_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.learn_corrections_enabled = enabled;
    settings::write_settings(&app, settings);
    if !enabled {
        super::session::cancel_session();
    }
    Ok(())
}

/// Sub-switch: whether the post-paste watcher learns from edits. The
/// dictionary keeps applying while this is off.
#[tauri::command]
#[specta::specta]
pub fn change_learn_from_edits_enabled_setting(
    app: AppHandle,
    enabled: bool,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.learn_from_edits_enabled = enabled;
    settings::write_settings(&app, settings);
    if !enabled {
        super::session::cancel_session();
    }
    Ok(())
}

/// How permissive the learning gates are (`conservative` | `balanced` |
/// `aggressive`). An unknown value falls back to `conservative`.
#[tauri::command]
#[specta::specta]
pub fn change_learn_corrections_aggressiveness_setting(
    app: AppHandle,
    aggressiveness: String,
) -> Result<(), String> {
    let parsed = match aggressiveness.as_str() {
        "conservative" => Aggressiveness::Conservative,
        "balanced" => Aggressiveness::Balanced,
        "aggressive" => Aggressiveness::Aggressive,
        other => {
            warn!(
                "Invalid learn-corrections aggressiveness '{}', defaulting to conservative",
                other
            );
            Aggressiveness::Conservative
        }
    };
    let mut settings = settings::get_settings(&app);
    settings.learn_corrections_aggressiveness = parsed;
    settings::write_settings(&app, settings);
    Ok(())
}

/// Post-paste learning window in seconds (clamped to 10–300 by the session).
#[tauri::command]
#[specta::specta]
pub fn change_learn_corrections_window_secs_setting(
    app: AppHandle,
    window_secs: u32,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    settings.learn_corrections_window_secs = window_secs;
    settings::write_settings(&app, settings);
    Ok(())
}

/// The full learned list (suggestions and active pairs) plus the block list.
#[tauri::command]
#[specta::specta]
pub fn get_learned_corrections(app: AppHandle) -> LearnedCorrections {
    (*store::get(&app)).clone()
}

/// Add (or overwrite) a pair by hand: active immediately, replaces any entry
/// for the same misheard text, lifts a block on the same pair.
#[tauri::command]
#[specta::specta]
pub fn add_learned_correction(
    app: AppHandle,
    misheard: String,
    intended: String,
) -> Result<LearnedCorrection, String> {
    let misheard = misheard.trim();
    let intended = intended.trim();
    if misheard.is_empty() || intended.is_empty() {
        return Err("misheard and intended text must not be empty".to_string());
    }
    Ok(store::update(&app, |data| {
        (data.add_manual(misheard, intended, now()), true)
    }))
}

/// Confirm a suggestion: it becomes active and is applied from now on.
#[tauri::command]
#[specta::specta]
pub fn accept_learned_correction(app: AppHandle, id: String) -> Result<(), String> {
    store::update(&app, |data| {
        let changed = data.accept(&id);
        (changed, changed)
    });
    Ok(())
}

/// Undo / reject: remove every listed pair; auto-learned ones are also blocked
/// so they are never learned again. The toast's Undo passes its whole group.
#[tauri::command]
#[specta::specta]
pub fn reject_learned_corrections(app: AppHandle, ids: Vec<String>) -> Result<(), String> {
    store::update(&app, |data| {
        let removed = data.reject(&ids, now());
        (removed, removed > 0)
    });
    Ok(())
}

/// Delete a pair without blocking it (it may be learned again).
#[tauri::command]
#[specta::specta]
pub fn remove_learned_correction(app: AppHandle, id: String) -> Result<(), String> {
    store::update(&app, |data| {
        let changed = data.remove(&id);
        (changed, changed)
    });
    Ok(())
}

/// Pause or resume an active pair without deleting it.
#[tauri::command]
#[specta::specta]
pub fn set_learned_correction_enabled(
    app: AppHandle,
    id: String,
    enabled: bool,
) -> Result<(), String> {
    store::update(&app, |data| {
        let changed = data.set_enabled(&id, enabled);
        (changed, changed)
    });
    Ok(())
}

/// Lift a block so the pair can be learned again.
#[tauri::command]
#[specta::specta]
pub fn unblock_learned_correction(app: AppHandle, id: String) -> Result<(), String> {
    store::update(&app, |data| {
        let changed = data.unblock(&id);
        (changed, changed)
    });
    Ok(())
}
