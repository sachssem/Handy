use crate::actions::ACTION_MAP;
use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::recording_limit_for_model_id;
use crate::settings::get_settings;
use log::{debug, error, warn};
use serde::Serialize;
use std::sync::mpsc::{self, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::{AppHandle, Emitter, Manager};

const DEBOUNCE: Duration = Duration::from_millis(30);

#[derive(Clone, Debug, Serialize)]
struct RecordingLimitEvent {
    deadline_epoch_ms: u64,
    limit_ms: u64,
    warning_ms: u64,
}

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input {
        binding_id: String,
        hotkey_string: String,
        is_pressed: bool,
        push_to_talk: bool,
    },
    Cancel {
        recording_was_active: bool,
    },
    AutoStop {
        session_id: u64,
    },
    ProcessingFinished,
}

/// Pipeline lifecycle, owned exclusively by the coordinator thread.
enum Stage {
    Idle,
    Recording { binding_id: String, session_id: u64 },
    Processing,
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline.
pub struct TranscriptionCoordinator {
    tx: Sender<Command>,
}

pub fn is_transcribe_binding(id: &str) -> bool {
    id == "transcribe" || id == "transcribe_with_post_process"
}

impl TranscriptionCoordinator {
    pub fn new(app: AppHandle) -> Self {
        let (tx, rx) = mpsc::channel();
        let worker_tx = tx.clone();

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut stage = Stage::Idle;
                let mut last_press: Option<Instant> = None;
                let mut next_session_id: u64 = 1;
                let tx = worker_tx.clone();

                while let Ok(cmd) = rx.recv() {
                    match cmd {
                        Command::Input {
                            binding_id,
                            hotkey_string,
                            is_pressed,
                            push_to_talk,
                        } => {
                            // Debounce rapid-fire press events (key repeat / double-tap).
                            // Releases always pass through for push-to-talk.
                            if is_pressed {
                                let now = Instant::now();
                                if last_press.is_some_and(|t| now.duration_since(t) < DEBOUNCE) {
                                    debug!("Debounced press for '{binding_id}'");
                                    continue;
                                }
                                last_press = Some(now);
                            }

                            if push_to_talk {
                                if is_pressed && matches!(stage, Stage::Idle) {
                                    start(
                                        &app,
                                        &mut stage,
                                        &tx,
                                        &mut next_session_id,
                                        &binding_id,
                                        &hotkey_string,
                                    );
                                } else if !is_pressed
                                    && matches!(&stage, Stage::Recording { binding_id: id, .. } if id == &binding_id)
                                {
                                    stop(&app, &mut stage, &binding_id, &hotkey_string);
                                }
                            } else if is_pressed {
                                match &stage {
                                    Stage::Idle => {
                                        start(
                                            &app,
                                            &mut stage,
                                            &tx,
                                            &mut next_session_id,
                                            &binding_id,
                                            &hotkey_string,
                                        );
                                    }
                                    Stage::Recording { binding_id: id, .. }
                                        if id == &binding_id =>
                                    {
                                        stop(&app, &mut stage, &binding_id, &hotkey_string);
                                    }
                                    _ => {
                                        debug!("Ignoring press for '{binding_id}': pipeline busy")
                                    }
                                }
                            }
                        }
                        Command::Cancel {
                            recording_was_active,
                        } => {
                            // Don't reset during processing — wait for the pipeline to finish.
                            if !matches!(stage, Stage::Processing)
                                && (recording_was_active
                                    || matches!(stage, Stage::Recording { .. }))
                            {
                                stage = Stage::Idle;
                            }
                        }
                        Command::AutoStop { session_id } => {
                            if let Stage::Recording {
                                binding_id,
                                session_id: active_session_id,
                            } = &stage
                            {
                                if *active_session_id == session_id {
                                    let binding_id = binding_id.clone();
                                    debug!(
                                        "Auto-stopping recording session {session_id} before model limit"
                                    );
                                    stop(&app, &mut stage, &binding_id, "auto-stop");
                                } else {
                                    debug!(
                                        "Ignoring stale auto-stop for session {session_id}; active session is {active_session_id}"
                                    );
                                }
                            } else {
                                debug!(
                                    "Ignoring auto-stop for session {session_id}; not recording"
                                );
                            }
                        }
                        Command::ProcessingFinished => {
                            stage = Stage::Idle;
                        }
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {e:?}");
            }
        });

        Self { tx }
    }

    /// Send a keyboard/signal input event for a transcribe binding.
    /// For signal-based toggles, use `is_pressed: true` and `push_to_talk: false`.
    pub fn send_input(
        &self,
        binding_id: &str,
        hotkey_string: &str,
        is_pressed: bool,
        push_to_talk: bool,
    ) {
        if self
            .tx
            .send(Command::Input {
                binding_id: binding_id.to_string(),
                hotkey_string: hotkey_string.to_string(),
                is_pressed,
                push_to_talk,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self
            .tx
            .send(Command::Cancel {
                recording_was_active,
            })
            .is_err()
        {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_processing_finished(&self) {
        if self.tx.send(Command::ProcessingFinished).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

fn start(
    app: &AppHandle,
    stage: &mut Stage,
    tx: &Sender<Command>,
    next_session_id: &mut u64,
    binding_id: &str,
    hotkey_string: &str,
) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.start(app, binding_id, hotkey_string);
    if app
        .try_state::<Arc<AudioRecordingManager>>()
        .is_some_and(|a| a.is_recording())
    {
        let session_id = *next_session_id;
        *next_session_id = next_session_id.saturating_add(1);
        schedule_recording_limit(app, tx, session_id);
        *stage = Stage::Recording {
            binding_id: binding_id.to_string(),
            session_id,
        };
    } else {
        debug!("Start for '{binding_id}' did not begin recording; staying idle");
    }
}

fn stop(app: &AppHandle, stage: &mut Stage, binding_id: &str, hotkey_string: &str) {
    let Some(action) = ACTION_MAP.get(binding_id) else {
        warn!("No action in ACTION_MAP for '{binding_id}'");
        return;
    };
    action.stop(app, binding_id, hotkey_string);
    *stage = Stage::Processing;
}

fn schedule_recording_limit(app: &AppHandle, tx: &Sender<Command>, session_id: u64) {
    let settings = get_settings(app);
    if !settings.auto_stop_recording_on_limit {
        return;
    }

    let Some(limit) = recording_limit_for_model_id(&settings.selected_model) else {
        return;
    };

    let deadline_epoch_ms = SystemTime::now()
        .checked_add(Duration::from_millis(limit.max_recording_ms))
        .and_then(|deadline| deadline.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(u64::MAX);

    let event = RecordingLimitEvent {
        deadline_epoch_ms,
        limit_ms: limit.max_recording_ms,
        warning_ms: limit.warning_ms.min(limit.max_recording_ms),
    };
    if let Err(err) = app.emit_to("recording_overlay", "recording-limit", event) {
        debug!("Failed to emit recording limit event: {err}");
    }

    let tx = tx.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(limit.max_recording_ms));
        if tx.send(Command::AutoStop { session_id }).is_err() {
            warn!("Transcription coordinator channel closed before auto-stop");
        }
    });
}
