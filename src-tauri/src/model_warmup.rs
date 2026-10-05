//! Warm-up inference right after a transcribe-cpp model load (experimental).
//!
//! The first run after a load pays one-time costs — ggml compiles its Metal
//! compute pipelines lazily on first use, and the freshly mapped weights are
//! faulted in — so a reload at the press made the first dictation up to ~2x
//! slower. [`run`] decodes one second of silence on the just-loaded session
//! while the user is still speaking; its text is discarded and never journaled
//! as a dictation. The caller runs it inside the model-loading window
//! (`is_loading` set, engine mutex held), so a real transcription waits for it
//! and never runs concurrently on the same engine.
//!
//! Only transcribe-cpp (GGML/GGUF, Metal/Vulkan/CUDA) engines are warmed. The
//! ONNX engines (Parakeet, Moonshine, SenseVoice, GigaAM, Canary, Cohere) run
//! on onnxruntime, which builds its execution plan at session creation, so a
//! throwaway run there buys nothing.
//!
//! Kill switch: `HANDY_MODEL_WARMUP=0` skips the warm-up.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use log::{debug, warn};
use transcribe_cpp::{RunOptions, Session};

/// transcribe-cpp takes 16 kHz mono PCM.
const SAMPLE_RATE: usize = 16_000;
/// One second: long enough to run the encoder and a decode step, short enough
/// to finish in ~0.1–0.2 s on Metal.
const WARMUP_SECS: usize = 1;
/// `u64::MAX` = no warm-up since the last [`take_last_ms`].
const NONE: u64 = u64::MAX;

static LAST_WARMUP_MS: AtomicU64 = AtomicU64::new(NONE);

/// Whether the warm-up is enabled (`HANDY_MODEL_WARMUP=0` disables it).
pub fn enabled() -> bool {
    std::env::var("HANDY_MODEL_WARMUP").as_deref() != Ok("0")
}

/// Run one throwaway inference on `session`. Returns its duration, or `None`
/// when the run failed (logged; the model stays usable either way).
pub fn run(session: &mut Session) -> Option<Duration> {
    let silence = vec![0.0_f32; SAMPLE_RATE * WARMUP_SECS];
    let started = Instant::now();
    match session.run(&silence, &RunOptions::default()) {
        Ok(_) => {
            let elapsed = started.elapsed();
            debug!(
                "Model warm-up inference took {}ms ({}s of silence)",
                elapsed.as_millis(),
                WARMUP_SECS
            );
            LAST_WARMUP_MS.store(elapsed.as_millis() as u64, Ordering::Release);
            Some(elapsed)
        }
        Err(e) => {
            warn!("Model warm-up inference failed (ignored): {}", e);
            None
        }
    }
}

/// Duration of the warm-up since the last call, consumed so a later load
/// without a warm-up does not report a stale value.
pub fn take_last_ms() -> Option<u64> {
    match LAST_WARMUP_MS.swap(NONE, Ordering::AcqRel) {
        NONE => None,
        ms => Some(ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_warmup_is_consumed_once() {
        LAST_WARMUP_MS.store(123, Ordering::Release);
        assert_eq!(take_last_ms(), Some(123));
        assert_eq!(take_last_ms(), None);
    }
}
