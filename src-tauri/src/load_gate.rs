//! fork(voice-control): the overlay paints before the model loads.
//!
//! A press after the model was unloaded starts the load together with the
//! recording. Loading and warming up the model keeps the GPU busy, and the
//! overlay's webview then gets its first frame on screen only once that is
//! through, up to 3.7 s after the press. So the press shuts this gate, the
//! load waits at it, and the overlay's first frame opens it.
//!
//! The gate opens by itself at its deadline, so an overlay that never reports
//! a frame costs the load [`TIMEOUT`] and no more.

use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::debug;

/// How long a load waits for the overlay's first frame. The frame is on
/// screen about 70 ms after the press when nothing competes for the GPU.
const TIMEOUT: Duration = Duration::from_millis(400);

/// Until when the gate is shut; `None` is open.
static SHUT_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);
static OPENED: Condvar = Condvar::new();

fn gate() -> MutexGuard<'static, Option<Instant>> {
    SHUT_UNTIL.lock().unwrap_or_else(|e| e.into_inner())
}

/// The press: the overlay is about to be shown and a load may follow.
pub fn shut() {
    *gate() = Some(Instant::now() + TIMEOUT);
}

/// The overlay's first frame is on screen.
pub fn open() {
    *gate() = None;
    OPENED.notify_all();
}

/// Called by a model load before it starts. Returns at once unless a press
/// shut the gate and its overlay has not painted yet.
pub fn wait() {
    let started = Instant::now();
    let mut shut_until = gate();
    let mut waited = false;
    while let Some(deadline) = *shut_until {
        let Some(left) = deadline.checked_duration_since(Instant::now()) else {
            // Past the deadline: the gate is open for everyone after us too.
            *shut_until = None;
            break;
        };
        waited = true;
        shut_until = OPENED
            .wait_timeout(shut_until, left)
            .unwrap_or_else(|e| e.into_inner())
            .0;
    }
    drop(shut_until);
    if waited {
        debug!(
            "Model load waited {:?} for the overlay's first frame",
            started.elapsed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One test, because the gate is one static.
    #[test]
    fn a_load_waits_for_the_first_frame_and_no_longer_than_the_deadline() {
        // Open: no wait.
        let started = Instant::now();
        wait();
        assert!(started.elapsed() < Duration::from_millis(50));

        // Shut, then opened by the frame: the load goes on at once.
        shut();
        let opener = std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(60));
            open();
        });
        let started = Instant::now();
        wait();
        let waited = started.elapsed();
        opener.join().unwrap();
        assert!(waited >= Duration::from_millis(50), "waited {waited:?}");
        assert!(waited < TIMEOUT, "waited {waited:?}");

        // Shut and never opened: the deadline lets the load through.
        shut();
        let started = Instant::now();
        wait();
        let waited = started.elapsed();
        assert!(waited >= TIMEOUT - Duration::from_millis(20), "{waited:?}");
        assert!(waited < TIMEOUT + Duration::from_millis(300), "{waited:?}");
        // And it stays open.
        let started = Instant::now();
        wait();
        assert!(started.elapsed() < Duration::from_millis(50));
    }
}
