//! fork(voice-control): dictating again while an earlier dictation is still
//! being transcribed.
//!
//! A press during the transcription starts the next recording at once instead
//! of waiting for the text. The dictations then finish in the order they were
//! stopped: each one transcribes and pastes only after the one before it is
//! through its paste. While a newer recording owns the overlay and the tray,
//! an older dictation leaves both alone.
//!
//! A dictation is identified by its recording session (`actions`'
//! `RECORDING_SESSION` at its stop).

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use log::{debug, info, warn};
use tauri::{AppHandle, Emitter, Manager};
use tokio::sync::oneshot;

use crate::managers::audio::AudioRecordingManager;
use crate::managers::model::ModelManager;
use crate::managers::transcription::TranscriptionManager;
use crate::settings::get_settings;
use crate::transcription_coordinator::TranscriptionCoordinator;
use crate::tray::{set_tray_state, TrayIconState};

/// The overlay's event: how many dictations are stopped and not yet done.
const BACKLOG_EVENT: &str = "dictation-backlog";

/// How long a new recording waits for the one before it to hand over the
/// microphone and close its live decode's route. Both happen within
/// milliseconds of the stop; past this the recording starts anyway and is
/// transcribed in one run when it stops.
const HANDOVER_TIMEOUT: Duration = Duration::from_secs(2);

struct Queue {
    /// Sessions of the dictations that are stopped and not yet done, oldest
    /// first.
    open: Vec<u64>,
    /// Resolves when the newest of them is done.
    tail: Option<oneshot::Receiver<()>>,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue {
    open: Vec::new(),
    tail: None,
});

thread_local! {
    /// The dictation whose pipeline this thread is running ([`scoped`]).
    static SESSION: Cell<Option<u64>> = const { Cell::new(None) };
}

fn queue() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

fn emit_backlog(app: &AppHandle, count: usize) {
    let _ = app.emit_to("recording_overlay", BACKLOG_EVENT, count);
}

/// Whether a press during a transcription may start the next recording. Not
/// with a model that streams: the engine holds one stream, and the earlier
/// dictation is still finishing it.
pub fn allowed(app: &AppHandle) -> bool {
    let Some(tm) = app.try_state::<Arc<TranscriptionManager>>() else {
        return false;
    };
    if tm.is_streaming() {
        return false;
    }
    let selected = get_settings(app).selected_model;
    !app.try_state::<Arc<ModelManager>>()
        .and_then(|models| models.get_model_info(&selected))
        .is_some_and(|model| model.supports_streaming)
}

/// Called before a recording starts. With an earlier dictation still open,
/// waits until that one has stopped its recording and closed its live
/// decode's route, so this recording's audio can't reach the earlier decode.
pub fn wait_for_handover(app: &AppHandle) {
    if queue().open.is_empty() {
        return;
    }
    let rm = app.state::<Arc<AudioRecordingManager>>();
    let router = app.state::<Arc<TranscriptionManager>>().stream_router();
    let started = Instant::now();
    while rm.is_recording() || router.is_open() {
        if started.elapsed() >= HANDOVER_TIMEOUT {
            warn!("The previous dictation did not hand over within {HANDOVER_TIMEOUT:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    debug!(
        "Recording over a running transcription; handover took {:?}",
        started.elapsed()
    );
}

/// A stopped dictation's place in the order. Dropping it lets the next one
/// transcribe and paste.
pub struct Turn {
    app: AppHandle,
    session: u64,
    before: Option<oneshot::Receiver<()>>,
    _done: oneshot::Sender<()>,
}

impl Turn {
    /// Resolves once every dictation stopped before this one is done.
    pub async fn wait(&mut self) {
        if let Some(before) = self.before.take() {
            // An error is the sender's drop, which is how a turn ends.
            let _ = before.await;
        }
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let count = {
            let mut queue = queue();
            queue.open.retain(|session| *session != self.session);
            queue.open.len()
        };
        emit_backlog(&self.app, count);
    }
}

/// Take the stopped dictation `session`'s place in the order. Called from the
/// stop itself, so the order is the order of the stops.
pub fn enqueue(app: &AppHandle, session: u64) -> Turn {
    let (done, next) = oneshot::channel();
    let (before, count) = {
        let mut queue = queue();
        queue.open.push(session);
        (queue.tail.replace(next), queue.open.len())
    };
    emit_backlog(app, count);
    Turn {
        app: app.clone(),
        session,
        before,
        _done: done,
    }
}

/// Runs a dictation's pipeline as `session`: on every poll its thread knows
/// which dictation it works for ([`stale`], [`keeps_model`]), and the
/// journal's id-less hooks write into `journal_id`'s record instead of the
/// newest one.
pub fn scoped<F: Future>(session: u64, journal_id: Option<u64>, future: F) -> Scoped<F> {
    Scoped {
        session,
        journal_id,
        future: Box::pin(future),
    }
}

pub struct Scoped<F> {
    session: u64,
    journal_id: Option<u64>,
    future: Pin<Box<F>>,
}

impl<F: Future> Future for Scoped<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<F::Output> {
        struct Restore(Option<u64>);
        impl Drop for Restore {
            fn drop(&mut self) {
                SESSION.with(|s| s.set(self.0));
            }
        }
        let _restore = Restore(SESSION.with(|s| s.replace(Some(self.session))));
        let journal_id = self.journal_id;
        crate::journal::with_scope(journal_id, || self.future.as_mut().poll(cx))
    }
}

/// Whether this thread runs the pipeline of a dictation that a newer
/// recording has taken the overlay and the tray from.
pub fn stale() -> bool {
    SESSION
        .with(Cell::get)
        .is_some_and(|session| session != crate::actions::recording_session())
}

/// Whether the model has to stay loaded although this thread's dictation is
/// through with it: another recording runs, or another dictation waits.
pub fn keeps_model(app: &AppHandle) -> bool {
    let own = SESSION.with(Cell::get);
    let others_open = queue().open.iter().any(|session| Some(*session) != own);
    others_open
        || app
            .try_state::<Arc<AudioRecordingManager>>()
            .is_some_and(|rm| rm.is_recording())
}

/// A cancel while a recording runs over an earlier dictation's transcription
/// ends only the recording; the earlier dictations still finish, and the
/// newest of them gets the overlay back. `false` when that is not the
/// situation and the caller's cancel of everything applies.
pub fn cancel_recording_only(app: &AppHandle) -> bool {
    let recording = crate::actions::recording_session();
    let newest_open = {
        let queue = queue();
        // The recording's own session is in the queue from its stop on.
        if queue.open.contains(&recording) {
            return false;
        }
        match queue.open.last() {
            Some(session) => *session,
            None => return false,
        }
    };
    let rm = app.state::<Arc<AudioRecordingManager>>();
    if !rm.is_recording() {
        return false;
    }
    info!("Cancelling the recording; the dictation before it keeps transcribing");
    rm.discard_recording();
    // The earlier dictations' routes are closed, so this is the recording's.
    app.state::<Arc<TranscriptionManager>>().cancel_stream();
    crate::actions::hand_ui_to(newest_open);
    set_tray_state(app, TrayIconState::Transcribing);
    crate::overlay::show_transcribing_overlay(app);
    if let Some(coordinator) = app.try_state::<TranscriptionCoordinator>() {
        coordinator.notify_cancel(true);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// The turns of `enqueue`, without an app to emit to.
    fn chain(
        tail: &mut Option<oneshot::Receiver<()>>,
    ) -> (Option<oneshot::Receiver<()>>, oneshot::Sender<()>) {
        let (done, next) = oneshot::channel();
        (tail.replace(next), done)
    }

    #[test]
    fn a_turn_waits_for_the_one_before_it_and_no_longer() {
        let mut tail = None;
        let (first_before, first_done) = chain(&mut tail);
        let (second_before, _second_done) = chain(&mut tail);
        assert!(
            first_before.is_none(),
            "the first dictation waits for nothing"
        );

        let mut second_before = second_before.expect("the second waits for the first");
        assert!(
            matches!(
                second_before.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ),
            "the first is not done yet"
        );
        drop(first_done);
        assert!(
            matches!(
                second_before.try_recv(),
                Err(oneshot::error::TryRecvError::Closed)
            ),
            "the first's end releases the second"
        );
    }

    #[test]
    fn a_scoped_pipeline_knows_its_session_only_while_it_is_polled() {
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        let future = scoped(7, None, async {
            SEEN.store(
                SESSION.with(Cell::get).unwrap_or(0) as usize,
                Ordering::SeqCst,
            );
        });
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        assert!(future.as_mut().poll(&mut cx).is_ready());
        assert_eq!(SEEN.load(Ordering::SeqCst), 7);
        assert_eq!(SESSION.with(Cell::get), None);
    }
}
