//! Keyboard shortcuts for the learned-correction toast (fork feature:
//! voice-control).
//!
//! Accept (default ⌃↩, plus the keypad Enter twin), Undo (default ⌃⌫) and a
//! fixed ⌃⎋ (dismiss) act on the visible toast without reaching for the mouse
//! — the toast panel never takes focus, so even dismiss has to go through the
//! global shortcut backend. Dismiss is ⌃⎋ rather than bare Esc so plain Esc
//! keeps reaching the focused app; it matches the usual recording-cancel combo. They are **transient**: registered only while a
//! toast is on screen, so the combos stay free for every other app the rest of
//! the time. With the handy-keys backend (blocking mode) the combo is
//! swallowed for the frontmost app while registered — acceptable precisely
//! because that window is a few seconds.
//!
//! ## Lifecycle
//!
//! Same shape as the recording cancel shortcut (`shortcut::register_cancel_*`):
//!
//! - **desired state** — [`TARGET`], written synchronously: [`arm`] on a
//!   successful reveal, [`disarm`] on every hide (webview dismissal, failsafe,
//!   window destruction) and when a shortcut claims the toast;
//! - **registered state** — [`REGISTERED`], whose lock also serializes the
//!   reconciliation passes;
//! - **reconciliation** — [`schedule_reconcile`] applies the *latest* desired
//!   state off the calling thread (the handy-keys handler runs on its manager
//!   thread, which must never wait on its own registration channel).
//!
//! Which slots are wanted follows the toast's content: Accept only when the
//! toast carries suggestions, Undo only when it carries learned (promoted)
//! pairs, dismiss always. Dismissing changes nothing in the store: a
//! suggestion stays a suggestion and can still be blocked from the settings
//! list. The cancel shortcut is only registered while recording; when a
//! recording starts with a toast on screen and its cancel binding resolves to
//! the dismiss combo, dismiss yields (the backends refuse a duplicate combo,
//! and cancelling the recording wins) and is re-armed once the recording
//! stops, if the toast is still visible.
//!
//! ## Generation guard
//!
//! Every reveal has a generation ([`super::toast`]). A press claims the
//! *current* target atomically (take-once), so a double press or a press that
//! races a newer reveal can never act twice or on the wrong pairs; the
//! follow-up dismissal is scoped to the claimed toast's generation and id, so
//! it cannot close a newer toast either.

use super::{commands, toast, LearnedCorrectionEvent};
use crate::settings::{self, AppSettings, KeyboardImplementation, ShortcutBinding};
use crate::shortcut::{handy_keys, tauri_impl};
use log::{debug, warn};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeSet;
use std::sync::Mutex;
use tauri::AppHandle;

/// Default Accept combo. `enter` parses in both backends (handy-keys maps it
/// to Return, global-hotkey to Enter — the same main key on a Mac).
pub const DEFAULT_ACCEPT: &str = "ctrl+enter";
/// Default Undo combo.
pub const DEFAULT_DISMISS: &str = "ctrl+backspace";
/// The fixed dismiss combo (not configurable). Parses in both backends; the
/// toast's `⌃esc` chip shows it (`DISMISS_SHORTCUT` in LearnedToast.tsx).
const DISMISS: &str = "ctrl+escape";

/// Which toast shortcut a setting / command refers to. `Dismiss` is the Undo
/// combo (`learned_toast_dismiss_shortcut`); the name predates the fixed
/// dismiss combo and is kept so stored settings and the bindings stay stable.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Type)]
#[serde(rename_all = "snake_case")]
pub enum LearnedToastShortcut {
    Accept,
    Dismiss,
}

/// A registration slot. `AcceptKeypad` is derived from the Accept combo when
/// its key is Enter, so the keypad Enter key works too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Accept,
    AcceptKeypad,
    Undo,
    Dismiss,
}

/// One entry per [`Slot`].
const SLOTS: usize = 4;

impl Slot {
    const ALL: [Slot; SLOTS] = [Slot::Accept, Slot::AcceptKeypad, Slot::Undo, Slot::Dismiss];

    fn index(self) -> usize {
        match self {
            Slot::Accept => 0,
            Slot::AcceptKeypad => 1,
            Slot::Undo => 2,
            Slot::Dismiss => 3,
        }
    }

    /// Binding id the backends report back to the shared shortcut handler.
    fn binding_id(self) -> &'static str {
        match self {
            Slot::Accept => "learned_toast_accept",
            Slot::AcceptKeypad => "learned_toast_accept_keypad",
            Slot::Undo => "learned_toast_undo",
            Slot::Dismiss => "learned_toast_dismiss",
        }
    }

    fn from_binding_id(id: &str) -> Option<Slot> {
        Slot::ALL.into_iter().find(|slot| slot.binding_id() == id)
    }
}

/// The toast the shortcuts currently act on.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    generation: u64,
    toast_id: String,
    suggested_ids: Vec<String>,
    active_ids: Vec<String>,
}

impl Target {
    fn from_event(generation: u64, event: &LearnedCorrectionEvent) -> Self {
        Self {
            generation,
            toast_id: event.id.clone(),
            suggested_ids: event.suggested_ids.clone(),
            active_ids: event.active_ids.clone(),
        }
    }
}

/// What a press does — the same backend calls the toast buttons make.
#[derive(Debug, PartialEq)]
enum ToastAction {
    /// Accept: every suggestion of the group.
    Accept(Vec<String>),
    /// Undo the promoted pairs: reject + block.
    Undo(Vec<String>),
    /// Hide the toast; the store is left untouched.
    Dismiss,
}

/// Mirrors the toast buttons: Accept acts on the suggestions, Undo on the
/// promoted pairs, dismiss (⌃⎋, the `⌃esc` chip) only hides. `None` = the slot has
/// nothing to act on for this toast, so it is not registered at all — Undo on
/// a suggestion-only toast stays unregistered rather than doubling as
/// dismiss, so ⌃⌫ (delete word in many editors) is not swallowed for nothing.
fn action_for(slot: Slot, target: &Target) -> Option<ToastAction> {
    match slot {
        Slot::Accept | Slot::AcceptKeypad => (!target.suggested_ids.is_empty())
            .then(|| ToastAction::Accept(target.suggested_ids.clone())),
        Slot::Undo => {
            (!target.active_ids.is_empty()).then(|| ToastAction::Undo(target.active_ids.clone()))
        }
        Slot::Dismiss => Some(ToastAction::Dismiss),
    }
}

/// Claim the current target for a press: take-once, so a second press (or a
/// press racing the reconcile pass that unregisters) is a no-op. A slot with
/// nothing to act on leaves the target in place.
fn claim(current: &mut Option<Target>, slot: Slot) -> Option<(Target, ToastAction)> {
    let action = action_for(slot, current.as_ref()?)?;
    current.take().map(|target| (target, action))
}

// ============================================================================
// Binding parsing and validation (pure)
// ============================================================================

/// A combo in comparable form: side-agnostic modifier set + canonical key.
#[derive(Debug, PartialEq, Eq)]
struct Combo {
    modifiers: BTreeSet<&'static str>,
    key: Option<String>,
}

fn canonical_modifier(part: &str) -> Option<&'static str> {
    let base = part
        .strip_suffix("_left")
        .or_else(|| part.strip_suffix("_right"))
        .or_else(|| part.strip_suffix("left"))
        .or_else(|| part.strip_suffix("right"))
        .unwrap_or(part);
    match base {
        "ctrl" | "control" => Some("ctrl"),
        "alt" | "opt" | "option" => Some("alt"),
        "shift" => Some("shift"),
        "cmd" | "command" | "meta" | "super" | "win" | "windows" => Some("cmd"),
        "fn" | "function" => Some("fn"),
        _ => None,
    }
}

fn canonical_key(part: &str) -> String {
    match part {
        "return" => "enter",
        "esc" => "escape",
        "numenter" | "keypadenter" => "numpadenter",
        other => other,
    }
    .to_string()
}

/// Parse `ctrl+enter`-style strings. `None` for empty parts or more than one
/// main key.
fn parse_combo(raw: &str) -> Option<Combo> {
    let mut modifiers = BTreeSet::new();
    let mut key = None;
    for part in raw.split('+') {
        let part = part.trim().to_lowercase();
        if part.is_empty() {
            return None;
        }
        if let Some(modifier) = canonical_modifier(&part) {
            modifiers.insert(modifier);
        } else if key.replace(canonical_key(&part)).is_some() {
            return None;
        }
    }
    Some(Combo { modifiers, key })
}

/// The id of the first binding that resolves to the same combo as `raw`.
fn find_conflict<'a>(
    raw: &str,
    others: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Option<&'a str> {
    let combo = parse_combo(raw)?;
    others
        .into_iter()
        .find(|(_, other)| parse_combo(other).as_ref() == Some(&combo))
        .map(|(id, _)| id)
}

/// Every global binding, the fixed dismiss combo, plus — for Undo — the Accept
/// combo (Accept wins a tie, so a hand-edited settings file cannot make one
/// press do both).
fn reserved_bindings(settings: &AppSettings, which: LearnedToastShortcut) -> Vec<(&str, &str)> {
    let mut reserved: Vec<(&str, &str)> = settings
        .bindings
        .iter()
        .map(|(id, binding)| (id.as_str(), binding.current_binding.as_str()))
        .collect();
    if which == LearnedToastShortcut::Dismiss {
        reserved.push((
            Slot::Accept.binding_id(),
            settings.learned_toast_accept_shortcut.as_str(),
        ));
    }
    // The fixed dismiss combo is never available to Accept / Undo.
    reserved.push((Slot::Dismiss.binding_id(), DISMISS));
    reserved
}

/// Validate a toast combo. Errors are markers the settings UI localizes:
/// `invalid`, `needs-modifier`, `conflict:<binding id>`. Returns the
/// normalized (trimmed, lowercase) string to store.
fn validate_binding<'a>(
    raw: &str,
    reserved: impl IntoIterator<Item = (&'a str, &'a str)>,
) -> Result<String, String> {
    let normalized = raw.trim().to_lowercase();
    let combo = parse_combo(&normalized).ok_or("invalid")?;
    if combo.key.is_none() {
        return Err("invalid".into());
    }
    // Bare keys and Shift-only combos are ordinary typing; swallowing them
    // (letters, digits or punctuation) must never be allowed.
    if !combo.modifiers.iter().any(|modifier| *modifier != "shift") {
        return Err("needs-modifier".into());
    }
    // Must register with either backend, so switching the keyboard
    // implementation can never strand it.
    tauri_impl::validate_shortcut(&normalized).map_err(|_| "invalid")?;
    handy_keys::validate_shortcut(&normalized).map_err(|_| "invalid")?;
    if let Some(id) = find_conflict(&normalized, reserved) {
        return Err(format!("conflict:{}", id));
    }
    Ok(normalized)
}

/// The keypad-Enter twin of an Enter combo, in the active backend's key name.
fn keypad_twin(raw: &str, implementation: KeyboardImplementation) -> Option<String> {
    let combo = parse_combo(raw)?;
    if combo.key.as_deref() != Some("enter") {
        return None;
    }
    let key = match implementation {
        KeyboardImplementation::Tauri => "numpadenter",
        KeyboardImplementation::HandyKeys => "keypadenter",
    };
    let mut parts: Vec<&str> = combo
        .modifiers
        .iter()
        .map(|modifier| match *modifier {
            "alt" => "option",
            "cmd" => "command",
            other => other,
        })
        .collect();
    parts.push(key);
    Some(parts.join("+"))
}

/// Whether the fixed dismiss combo must stay unregistered: another binding
/// resolves to it. The recording cancel binding only counts while `recording`
/// (it is registered for the recording's duration only); the toast's own
/// combos are refused it at validation (see [`reserved_bindings`]).
fn dismiss_conflict(settings: &AppSettings, recording: bool) -> Option<&str> {
    find_conflict(
        DISMISS,
        settings
            .bindings
            .iter()
            .filter(|(id, _)| recording || id.as_str() != "cancel")
            .map(|(id, binding)| (id.as_str(), binding.current_binding.as_str())),
    )
}

/// The combo each slot should hold for `target` (`None` = not registered).
/// `recording` = the recording cancel shortcut is wanted right now.
fn desired_bindings(
    target: Option<&Target>,
    settings: &AppSettings,
    recording: bool,
) -> [Option<String>; SLOTS] {
    let mut desired: [Option<String>; SLOTS] = Default::default();
    let Some(target) = target else {
        return desired;
    };
    for slot in Slot::ALL {
        if action_for(slot, target).is_none() {
            continue;
        }
        if slot == Slot::Dismiss {
            match dismiss_conflict(settings, recording) {
                None => desired[slot.index()] = Some(DISMISS.to_string()),
                Some(id) => debug!("learned-toast dismiss yields to binding '{}'", id),
            }
            continue;
        }
        let (which, raw) = match slot {
            Slot::Accept => (
                LearnedToastShortcut::Accept,
                settings.learned_toast_accept_shortcut.clone(),
            ),
            Slot::AcceptKeypad => {
                match keypad_twin(
                    &settings.learned_toast_accept_shortcut,
                    settings.keyboard_implementation,
                ) {
                    Some(twin) => (LearnedToastShortcut::Accept, twin),
                    None => continue,
                }
            }
            Slot::Undo => (
                LearnedToastShortcut::Dismiss,
                settings.learned_toast_dismiss_shortcut.clone(),
            ),
            Slot::Dismiss => continue,
        };
        // The keypad twin is backend-specific (and global-hotkey's name fails
        // the handy-keys parser), so only the conflict check applies to it.
        let checked = if slot == Slot::AcceptKeypad {
            match find_conflict(&raw, reserved_bindings(settings, which)) {
                Some(id) => Err(format!("conflict:{}", id)),
                None => Ok(raw.clone()),
            }
        } else {
            validate_binding(&raw, reserved_bindings(settings, which))
        };
        match checked {
            Ok(binding) => desired[slot.index()] = Some(binding),
            Err(reason) => warn!(
                "learned-toast shortcut '{}' ({}) not registered: {}",
                slot.binding_id(),
                raw,
                reason
            ),
        }
    }
    desired
}

// ============================================================================
// State and registration
// ============================================================================

/// Desired state keeps suspension and the target under one lock: a press
/// cannot claim a toast during shortcut capture, even before unregistering.
#[derive(Default)]
struct ShortcutState {
    target: Option<Target>,
    suspended: bool,
}

impl ShortcutState {
    fn visible_target(&self) -> Option<&Target> {
        self.target.as_ref().filter(|_| !self.suspended)
    }

    fn claim(&mut self, slot: Slot) -> Option<(Target, ToastAction)> {
        if self.suspended {
            return None;
        }
        claim(&mut self.target, slot)
    }
}

static TARGET: Mutex<ShortcutState> = Mutex::new(ShortcutState {
    target: None,
    suspended: false,
});

struct Registration {
    implementation: KeyboardImplementation,
    binding: ShortcutBinding,
}

/// Registered state per [`Slot`]. Remembers the backend it went to, so an
/// implementation switch mid-toast still unregisters from the right one.
static REGISTERED: Mutex<[Option<Registration>; SLOTS]> = Mutex::new([None, None, None, None]);

/// Arm with the event captured by this reveal's main-thread closure; a later
/// reveal's staging can never be consumed by an earlier reveal.
pub(super) fn arm(app: &AppHandle, generation: u64, event: &LearnedCorrectionEvent) {
    if let Ok(mut state) = TARGET.lock() {
        state.target = Some(Target::from_event(generation, event));
    }
    schedule_reconcile(app);
}

/// The toast is gone (or going): stop acting on it and release the combos.
pub(super) fn disarm(app: &AppHandle) {
    let was_armed = TARGET
        .lock()
        .map(|mut state| state.target.take().is_some())
        .unwrap_or(false);
    if was_armed {
        schedule_reconcile(app);
    }
}

/// Release transient shortcuts before settings capture starts. Reconcile
/// synchronously so the command does not return with Ctrl+Enter still swallowed.
pub fn suspend(app: &AppHandle) {
    TARGET.lock().unwrap_or_else(|e| e.into_inner()).suspended = true;
    reconcile(app);
}

/// Restore the current toast after capture, unless it was dismissed meanwhile.
pub fn resume(app: &AppHandle) {
    TARGET.lock().unwrap_or_else(|e| e.into_inner()).suspended = false;
    schedule_reconcile(app);
}

/// The recording cancel shortcut is about to be registered: release dismiss
/// now, synchronously, so the cancel registration does not hit a duplicate
/// combo. Called by the cancel reconciliation after the recording asked for it.
pub fn yield_dismiss_to_cancel(app: &AppHandle) {
    reconcile(app);
}

/// The recording cancel shortcut was released: re-arm dismiss if a toast is
/// still on screen.
pub fn reclaim_dismiss_from_cancel(app: &AppHandle) {
    schedule_reconcile(app);
}

/// Apply the desired state off the calling thread; each pass applies the
/// latest request, so spawned passes may run in any order.
fn schedule_reconcile(app: &AppHandle) {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        reconcile(&app);
    });
}

fn reconcile(app: &AppHandle) {
    // Dynamic registration is unstable on Linux (see the cancel shortcut); the
    // toast never shows there anyway.
    #[cfg(target_os = "linux")]
    {
        let _ = app;
        return;
    }

    #[cfg(not(target_os = "linux"))]
    {
        let mut registered = REGISTERED.lock().unwrap_or_else(|e| e.into_inner());
        let target = TARGET
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .visible_target()
            .cloned();
        let settings = settings::get_settings(app);
        let implementation = settings.keyboard_implementation;
        let recording = crate::shortcut::cancel_shortcut_requested();
        let desired = desired_bindings(target.as_ref(), &settings, recording);

        for slot in Slot::ALL {
            let wanted = desired[slot.index()].as_ref();
            let current = &mut registered[slot.index()];
            let up_to_date = match (current.as_ref(), wanted) {
                (None, None) => true,
                (Some(reg), Some(binding)) => {
                    reg.implementation == implementation && reg.binding.current_binding == *binding
                }
                _ => false,
            };
            if up_to_date {
                continue;
            }
            if let Some(reg) = current.take() {
                let result = match reg.implementation {
                    KeyboardImplementation::Tauri => {
                        tauri_impl::unregister_shortcut(app, reg.binding.clone())
                    }
                    KeyboardImplementation::HandyKeys => {
                        handy_keys::unregister_shortcut(app, reg.binding.clone())
                    }
                };
                match result {
                    Ok(()) => debug!("learned-toast shortcut released: {}", slot.binding_id()),
                    Err(e) => warn!(
                        "Failed to unregister learned-toast shortcut '{}': {}",
                        slot.binding_id(),
                        e
                    ),
                }
            }
            let Some(binding) = wanted else {
                continue;
            };
            let binding = ShortcutBinding {
                id: slot.binding_id().to_string(),
                name: slot.binding_id().to_string(),
                description: "Learned-correction toast shortcut".to_string(),
                default_binding: binding.clone(),
                current_binding: binding.clone(),
            };
            let result = match implementation {
                KeyboardImplementation::Tauri => {
                    tauri_impl::register_shortcut(app, binding.clone())
                }
                KeyboardImplementation::HandyKeys => {
                    handy_keys::register_shortcut(app, binding.clone())
                }
            };
            match result {
                Ok(()) => {
                    debug!(
                        "learned-toast shortcut armed: {} = {}",
                        slot.binding_id(),
                        binding.current_binding
                    );
                    *current = Some(Registration {
                        implementation,
                        binding,
                    });
                }
                Err(e) => warn!(
                    "Failed to register learned-toast shortcut '{}' ({}): {}",
                    slot.binding_id(),
                    binding.current_binding,
                    e
                ),
            }
        }
    }
}

/// Hook for the shared shortcut handler (`shortcut::handler`): returns `true`
/// when `binding_id` is a toast shortcut (handled here, never an ACTION_MAP
/// action). Runs on the backend's event thread, so the work is spawned.
pub fn handle_shortcut_event(app: &AppHandle, binding_id: &str, is_pressed: bool) -> bool {
    let Some(slot) = Slot::from_binding_id(binding_id) else {
        return false;
    };
    if !is_pressed {
        return true;
    }
    let claimed = TARGET.lock().ok().and_then(|mut state| state.claim(slot));
    let Some((target, action)) = claimed else {
        debug!("learned-toast shortcut {}: no toast to act on", binding_id);
        return true;
    };
    debug!(
        "learned-toast shortcut {}: {:?} (toast generation {})",
        binding_id, action, target.generation
    );
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        run_action(&app, action);
        toast::dismiss_after_shortcut(&app, target.generation, &target.toast_id);
        // The claim cleared the target: release the combos.
        reconcile(&app);
    });
    true
}

fn run_action(app: &AppHandle, action: ToastAction) {
    match action {
        ToastAction::Accept(ids) => {
            for id in ids {
                if let Err(e) = commands::accept_learned_correction(app.clone(), id) {
                    warn!("learned-toast shortcut: accept failed: {}", e);
                }
            }
        }
        ToastAction::Undo(ids) => {
            if let Err(e) = commands::reject_learned_corrections(app.clone(), ids) {
                warn!("learned-toast shortcut: undo failed: {}", e);
            }
        }
        // Only hides (the caller dismisses the toast); nothing to persist.
        ToastAction::Dismiss => {}
    }
}

/// Change (or, with `binding: None`, reset) a toast shortcut. Refuses combos
/// that are unparseable, lack a modifier, or collide with another Handy
/// shortcut; the error is a marker the settings UI localizes.
#[tauri::command]
#[specta::specta]
pub fn change_learned_toast_shortcut_setting(
    app: AppHandle,
    action: LearnedToastShortcut,
    binding: Option<String>,
) -> Result<(), String> {
    let mut settings = settings::get_settings(&app);
    let raw = binding.unwrap_or_else(|| match action {
        LearnedToastShortcut::Accept => DEFAULT_ACCEPT.to_string(),
        LearnedToastShortcut::Dismiss => DEFAULT_DISMISS.to_string(),
    });
    let mut reserved = reserved_bindings(&settings, action);
    // Accept must not take the Undo combo either (the runtime tie-break only
    // covers hand-edited settings).
    if action == LearnedToastShortcut::Accept {
        reserved.push((
            Slot::Undo.binding_id(),
            settings.learned_toast_dismiss_shortcut.as_str(),
        ));
    }
    let normalized = validate_binding(&raw, reserved)?;
    match action {
        LearnedToastShortcut::Accept => settings.learned_toast_accept_shortcut = normalized,
        LearnedToastShortcut::Dismiss => settings.learned_toast_dismiss_shortcut = normalized,
    }
    settings::write_settings(&app, settings);
    // Picks the new combo up if a toast is on screen right now.
    schedule_reconcile(&app);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(generation: u64, suggested: &[&str], active: &[&str]) -> Target {
        Target {
            generation,
            toast_id: format!("toast-{}", generation),
            suggested_ids: suggested.iter().map(|s| s.to_string()).collect(),
            active_ids: active.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn suspend_blocks_claims_and_registrations_and_resume_restores_the_current_toast() {
        let mut state = ShortcutState {
            target: Some(target(1, &["old"], &[])),
            suspended: true,
        };
        assert!(state.visible_target().is_none());
        assert!(state.claim(Slot::Accept).is_none());
        // A reveal during capture is retained, but remains inactive.
        state.target = Some(target(2, &["new"], &[]));
        assert!(state.visible_target().is_none());
        state.suspended = false;
        assert_eq!(state.visible_target().unwrap().generation, 2);
        assert_eq!(
            state.claim(Slot::Accept).unwrap().1,
            ToastAction::Accept(ids(&["new"]))
        );
        assert!(state.claim(Slot::Accept).is_none());
        // Hiding during capture must not resurrect the old shortcuts.
        state.suspended = true;
        state.target = None;
        state.suspended = false;
        assert!(state.visible_target().is_none());
    }

    #[test]
    fn back_to_back_reveals_pair_each_generation_with_its_own_event() {
        let event = |id: &str| LearnedCorrectionEvent {
            id: id.into(),
            misheard: "from".into(),
            intended: "to".into(),
            status: super::super::CorrectionStatus::Suggested,
            suggested_ids: vec![id.into()],
            active_ids: Vec::new(),
            extra: 0,
        };
        let first = event("first");
        let second = event("second");
        let first_target = Target::from_event(1, &first);
        let second_target = Target::from_event(2, &second);
        assert_eq!(first_target, target_event(1, "first"));
        assert_eq!(second_target, target_event(2, "second"));
        fn target_event(generation: u64, id: &str) -> Target {
            Target {
                generation,
                toast_id: id.into(),
                suggested_ids: vec![id.into()],
                active_ids: Vec::new(),
            }
        }
    }

    #[test]
    fn validate_rejects_shift_only_typing_combos() {
        for combo in [
            "shift+a",
            "shift+1",
            "shift+slash",
            "shift+period",
            "shift+semicolon",
        ] {
            assert_eq!(
                validate_binding(combo, []),
                Err("needs-modifier".into()),
                "{combo}"
            );
        }
        assert_eq!(
            validate_binding("ctrl+shift+a", []),
            Ok("ctrl+shift+a".into())
        );
        assert_eq!(
            validate_binding("ctrl+enter", [("cancel", "ctrl+return")]),
            Err("conflict:cancel".into())
        );
    }

    #[test]
    fn suggestion_toast_maps_accept_and_dismiss_but_no_undo() {
        let t = target(1, &["a", "b"], &[]);
        assert_eq!(
            action_for(Slot::Accept, &t),
            Some(ToastAction::Accept(ids(&["a", "b"])))
        );
        assert_eq!(
            action_for(Slot::AcceptKeypad, &t),
            Some(ToastAction::Accept(ids(&["a", "b"])))
        );
        // Nothing to undo: the suggestion is never rejected from the toast.
        assert_eq!(action_for(Slot::Undo, &t), None);
        assert_eq!(action_for(Slot::Dismiss, &t), Some(ToastAction::Dismiss));
    }

    #[test]
    fn promoted_toast_maps_undo_and_dismiss_and_has_no_accept() {
        let t = target(1, &[], &["x"]);
        assert_eq!(action_for(Slot::Accept, &t), None);
        assert_eq!(action_for(Slot::AcceptKeypad, &t), None);
        assert_eq!(
            action_for(Slot::Undo, &t),
            Some(ToastAction::Undo(ids(&["x"])))
        );
        assert_eq!(action_for(Slot::Dismiss, &t), Some(ToastAction::Dismiss));
    }

    #[test]
    fn mixed_toast_undo_rejects_only_the_promoted_pairs() {
        let t = target(1, &["s"], &["x"]);
        assert_eq!(
            action_for(Slot::Undo, &t),
            Some(ToastAction::Undo(ids(&["x"])))
        );
    }

    #[test]
    fn dismiss_claims_once_without_touching_pairs() {
        let mut current = Some(target(6, &["s"], &["x"]));
        let (claimed, action) = claim(&mut current, Slot::Dismiss).expect("dismiss acts");
        assert_eq!(claimed.generation, 6);
        assert_eq!(action, ToastAction::Dismiss);
        assert!(current.is_none());
        assert!(claim(&mut current, Slot::Dismiss).is_none());
    }

    #[test]
    fn claim_is_take_once() {
        let mut current = Some(target(3, &["s"], &[]));
        let (claimed, _) = claim(&mut current, Slot::Accept).expect("first press acts");
        assert_eq!(claimed.generation, 3);
        assert!(current.is_none());
        assert!(
            claim(&mut current, Slot::Accept).is_none(),
            "second press is a no-op"
        );
    }

    #[test]
    fn claim_acts_on_the_newest_toast_only() {
        // A newer reveal replaced the target: the press carries the newer
        // generation, so the follow-up dismissal can never hit the old toast.
        let mut current = Some(target(4, &[], &["new"]));
        let (claimed, action) = claim(&mut current, Slot::Undo).unwrap();
        assert_eq!(claimed.generation, 4);
        assert_eq!(action, ToastAction::Undo(ids(&["new"])));
    }

    #[test]
    fn claim_without_an_action_keeps_the_toast_armed() {
        let mut current = Some(target(5, &[], &["x"]));
        assert!(claim(&mut current, Slot::Accept).is_none());
        assert!(current.is_some());
    }

    #[test]
    fn slot_ids_round_trip() {
        for slot in Slot::ALL {
            assert_eq!(Slot::from_binding_id(slot.binding_id()), Some(slot));
        }
        assert_eq!(Slot::from_binding_id("cancel"), None);
    }

    #[test]
    fn parse_combo_normalizes_aliases_and_sides() {
        assert_eq!(parse_combo("Ctrl+Return"), parse_combo("control+enter"));
        assert_eq!(parse_combo("ctrl_left+enter"), parse_combo("ctrl+enter"));
        assert_eq!(parse_combo("option+shift+a"), parse_combo("shift+alt+a"));
        assert_eq!(parse_combo("command+k"), parse_combo("cmd+k"));
        assert_eq!(parse_combo("ctrl+a+b"), None);
        assert_eq!(parse_combo("ctrl++a"), None);
    }

    #[test]
    fn conflicts_are_found_by_combo_not_spelling() {
        let others = [("transcribe", "option+space"), ("cancel", "Control+Esc")];
        assert_eq!(find_conflict("ctrl+escape", others), Some("cancel"));
        assert_eq!(find_conflict("ctrl+enter", others), None);
        // Ctrl+Escape (recording cancel) and the defaults never collide.
        assert_eq!(
            find_conflict(DEFAULT_ACCEPT, [("cancel", "ctrl+escape")]),
            None
        );
        assert_eq!(
            find_conflict(DEFAULT_DISMISS, [("cancel", "ctrl+escape")]),
            None
        );
    }

    #[test]
    fn validate_accepts_the_defaults() {
        assert_eq!(
            validate_binding(DEFAULT_ACCEPT, []),
            Ok("ctrl+enter".into())
        );
        assert_eq!(
            validate_binding(" Ctrl+Backspace ", []),
            Ok("ctrl+backspace".into())
        );
    }

    #[test]
    fn validate_rejects_bare_keys_modifier_only_and_conflicts() {
        assert_eq!(validate_binding("enter", []), Err("needs-modifier".into()));
        assert_eq!(validate_binding("ctrl+shift", []), Err("invalid".into()));
        assert_eq!(validate_binding("", []), Err("invalid".into()));
        // Side-specific modifiers fail the Tauri parser.
        assert_eq!(
            validate_binding("ctrl_left+enter", []),
            Err("invalid".into())
        );
        assert_eq!(
            validate_binding("ctrl+enter", [("transcribe", "ctrl+return")]),
            Err("conflict:transcribe".into())
        );
    }

    #[test]
    fn keypad_twin_only_for_enter_and_per_backend() {
        assert_eq!(
            keypad_twin("ctrl+enter", KeyboardImplementation::Tauri),
            Some("ctrl+numpadenter".into())
        );
        assert_eq!(
            keypad_twin("option+ctrl+return", KeyboardImplementation::HandyKeys),
            Some("option+ctrl+keypadenter".into())
        );
        assert_eq!(
            keypad_twin("ctrl+backspace", KeyboardImplementation::Tauri),
            None
        );
    }

    #[test]
    fn keypad_twins_parse_in_their_backend() {
        let tauri = keypad_twin("ctrl+enter", KeyboardImplementation::Tauri).unwrap();
        assert!(tauri_impl::validate_shortcut(&tauri).is_ok());
        let handy = keypad_twin("ctrl+enter", KeyboardImplementation::HandyKeys).unwrap();
        assert!(handy_keys::validate_shortcut(&handy).is_ok());
    }

    #[test]
    fn desired_bindings_follow_the_toast_content() {
        let mut settings = settings::get_default_settings();
        settings.keyboard_implementation = KeyboardImplementation::Tauri;
        assert_eq!(
            desired_bindings(None, &settings, false),
            [None, None, None, None]
        );

        let suggestion = target(1, &["s"], &[]);
        assert_eq!(
            desired_bindings(Some(&suggestion), &settings, false),
            [
                Some("ctrl+enter".into()),
                Some("ctrl+numpadenter".into()),
                None,
                Some("ctrl+escape".into())
            ]
        );

        let promoted = target(2, &[], &["x"]);
        assert_eq!(
            desired_bindings(Some(&promoted), &settings, false),
            [
                None,
                None,
                Some("ctrl+backspace".into()),
                Some("ctrl+escape".into())
            ]
        );
    }

    #[test]
    fn dismiss_yields_to_a_recording_cancel_on_the_same_combo() {
        let mut settings = settings::get_default_settings();
        let t = target(1, &["s"], &[]);
        // Default cancel (bare Esc) never collides with ⌃⎋.
        assert_eq!(
            desired_bindings(Some(&t), &settings, true)[Slot::Dismiss.index()],
            Some("ctrl+escape".into())
        );
        // A cancel on ⌃⎋ (side-specific spelling included) owns it while
        // recording; idle, the toast has it.
        if let Some(cancel) = settings.bindings.get_mut("cancel") {
            cancel.current_binding = "ctrl_left+escape".into();
        }
        assert_eq!(
            desired_bindings(Some(&t), &settings, true)[Slot::Dismiss.index()],
            None
        );
        assert_eq!(
            desired_bindings(Some(&t), &settings, false)[Slot::Dismiss.index()],
            Some("ctrl+escape".into())
        );
    }

    #[test]
    fn dismiss_combo_parses_in_both_backends_and_is_reserved() {
        assert!(tauri_impl::validate_shortcut(DISMISS).is_ok());
        assert!(handy_keys::validate_shortcut(DISMISS).is_ok());
        let settings = settings::get_default_settings();
        for which in [LearnedToastShortcut::Accept, LearnedToastShortcut::Dismiss] {
            assert_eq!(
                validate_binding("ctrl+escape", reserved_bindings(&settings, which)),
                Err("conflict:learned_toast_dismiss".into())
            );
        }
    }

    #[test]
    fn desired_bindings_refuse_a_global_conflict() {
        let mut settings = settings::get_default_settings();
        settings.keyboard_implementation = KeyboardImplementation::Tauri;
        if let Some(cancel) = settings.bindings.get_mut("cancel") {
            cancel.current_binding = "ctrl+backspace".into();
        }
        settings.learned_toast_dismiss_shortcut = "ctrl+backspace".into();
        let desired = desired_bindings(Some(&target(1, &["s"], &["x"])), &settings, false);
        assert_eq!(desired[Slot::Undo.index()], None);
        assert!(desired[Slot::Accept.index()].is_some());
    }

    #[test]
    fn undo_equal_to_accept_loses_the_tie() {
        let mut settings = settings::get_default_settings();
        settings.learned_toast_dismiss_shortcut = settings.learned_toast_accept_shortcut.clone();
        let desired = desired_bindings(Some(&target(1, &["s"], &["x"])), &settings, false);
        assert!(desired[Slot::Accept.index()].is_some());
        assert_eq!(desired[Slot::Undo.index()], None);
    }
}
