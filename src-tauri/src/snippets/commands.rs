//! Tauri commands for snippets (fork feature: voice-control).
//!
//! Granular add / update / remove so the UI never writes back a stale copy of
//! the whole list. Every change emits `settings-changed`, which makes the
//! frontend store re-read the settings.

use super::{trigger_words, Snippet};
use crate::settings;
use tauri::{AppHandle, Emitter};

fn emit_changed(app: &AppHandle) {
    let _ = app.emit(
        "settings-changed",
        serde_json::json!({ "setting": "snippets" }),
    );
}

/// Validate a trigger/expansion pair against `existing` (the snippet `id`
/// itself excluded). Returns the trimmed trigger.
fn validate(
    existing: &[Snippet],
    id: Option<&str>,
    trigger: &str,
    expansion: &str,
) -> Result<String, String> {
    let trigger = trigger.trim();
    let words = trigger_words(trigger);
    if words.is_empty() {
        return Err("The trigger needs at least one word".to_string());
    }
    if expansion.trim().is_empty() {
        return Err("The expansion is empty".to_string());
    }
    if existing
        .iter()
        .any(|s| Some(s.id.as_str()) != id && trigger_words(&s.trigger) == words)
    {
        return Err(format!("A snippet for '{}' already exists", trigger));
    }
    Ok(trigger.to_string())
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    format!(
        "snip-{:x}-{:x}",
        crate::journal::now_ms(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    )
}

#[tauri::command]
#[specta::specta]
pub fn add_snippet(app: AppHandle, trigger: String, expansion: String) -> Result<Snippet, String> {
    let mut settings = settings::get_settings(&app);
    let trigger = validate(&settings.snippets, None, &trigger, &expansion)?;
    let snippet = Snippet {
        id: new_id(),
        trigger,
        expansion,
        enabled: true,
    };
    settings.snippets.push(snippet.clone());
    settings::write_settings(&app, settings);
    emit_changed(&app);
    Ok(snippet)
}

/// Replace the snippet with `snippet.id` (trigger, expansion, enabled).
#[tauri::command]
#[specta::specta]
pub fn update_snippet(app: AppHandle, snippet: Snippet) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let trigger = validate(
        &settings.snippets,
        Some(&snippet.id),
        &snippet.trigger,
        &snippet.expansion,
    )?;
    let slot = settings
        .snippets
        .iter_mut()
        .find(|s| s.id == snippet.id)
        .ok_or_else(|| format!("Snippet {} not found", snippet.id))?;
    *slot = Snippet { trigger, ..snippet };
    settings::write_settings(&app, settings);
    emit_changed(&app);
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn remove_snippet(app: AppHandle, id: String) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let before = settings.snippets.len();
    settings.snippets.retain(|s| s.id != id);
    if settings.snippets.len() == before {
        return Err(format!("Snippet {} not found", id));
    }
    settings::write_settings(&app, settings);
    emit_changed(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snippet(id: &str, trigger: &str) -> Snippet {
        Snippet {
            id: id.to_string(),
            trigger: trigger.to_string(),
            expansion: "x".to_string(),
            enabled: true,
        }
    }

    #[test]
    fn rejects_empty_and_duplicate_triggers() {
        let existing = [snippet("a", "Meine Adresse")];
        assert!(validate(&existing, None, " ,. ", "x").is_err());
        assert!(validate(&existing, None, "neu", " ").is_err());
        assert!(validate(&existing, None, "meine, adresse", "x").is_err());
        // Updating the snippet itself keeps its trigger.
        assert_eq!(
            validate(&existing, Some("a"), " Meine Adresse ", "x").unwrap(),
            "Meine Adresse"
        );
        assert!(validate(&existing, None, "mein Calendly", "x").is_ok());
    }
}
