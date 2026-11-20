//! fork(voice-control): offline VAD (the batch dictation path) trims only the
//! edges of a recording.
//!
//! Leading silence is dropped as upstream does. After the first speech, quiet
//! frames are held in a pending gap and kept when speech resumes; a gap longer
//! than 3 s keeps only its first and last 750 ms. At stop, a trailing gap
//! beyond the VAD hangover is kept whole up to 1.5 s, longer ones keep their
//! first 700 ms, which protects quiet final words classified as noise. A
//! recording without confirmed speech stays empty. Streaming keeps upstream's
//! filtering. Fork-owned; the sink calls in at three marked points.

use std::borrow::Cow;

use crate::audio_toolkit::constants;

const SAMPLES_PER_SECOND: usize = constants::WHISPER_SAMPLE_RATE as usize;
/// Short trailing gaps (<= 1.5 s) stay whole.
const OFFLINE_FULL_TAIL_MAX_SAMPLES: usize = SAMPLES_PER_SECOND * 1500 / 1000;
/// Longer trailing gaps keep their first 700 ms.
const OFFLINE_LONG_TAIL_KEEP_SAMPLES: usize = SAMPLES_PER_SECOND * 700 / 1000;
/// Internal pauses up to 3 s stay whole.
const OFFLINE_FULL_INTERNAL_GAP_MAX_SAMPLES: usize = SAMPLES_PER_SECOND * 3;
/// Longer internal pauses keep both speech-adjacent edges.
const OFFLINE_INTERNAL_GAP_EDGE_SAMPLES: usize = SAMPLES_PER_SECOND * 750 / 1000;

/// A noise frame: held once speech was recorded (`recorded` non-empty),
/// dropped before (leading silence).
pub(super) fn on_noise(recorded: &[f32], pending_gap: &mut Vec<f32>, raw: &[f32]) {
    if !recorded.is_empty() {
        pending_gap.extend_from_slice(raw);
    }
}

/// What a speech frame adds to the recording. Before the first speech that is
/// the detector's output (with its pre-roll). After it, the pending gap
/// (capped) and only this raw frame: a resumed onset's pre-roll repeats frames
/// the gap already holds.
pub(super) fn on_speech<'a>(
    recorded: &[f32],
    pending_gap: &mut Vec<f32>,
    detected: &'a [f32],
    raw: &'a [f32],
) -> Cow<'a, [f32]> {
    if recorded.is_empty() {
        return Cow::Borrowed(detected);
    }
    if pending_gap.is_empty() {
        return Cow::Borrowed(raw);
    }
    let mut kept = Vec::with_capacity(
        pending_gap.len().min(2 * OFFLINE_INTERNAL_GAP_EDGE_SAMPLES) + raw.len(),
    );
    if pending_gap.len() > OFFLINE_FULL_INTERNAL_GAP_MAX_SAMPLES {
        kept.extend_from_slice(&pending_gap[..OFFLINE_INTERNAL_GAP_EDGE_SAMPLES]);
        kept.extend_from_slice(
            &pending_gap[pending_gap.len() - OFFLINE_INTERNAL_GAP_EDGE_SAMPLES..],
        );
    } else {
        kept.extend_from_slice(pending_gap);
    }
    pending_gap.clear();
    kept.extend_from_slice(raw);
    Cow::Owned(kept)
}

/// The stop tail to keep, taken out of `pending_gap` (whose allocation is
/// released). Empty when nothing was recorded.
pub(super) fn take_tail(recorded: &[f32], pending_gap: &mut Vec<f32>) -> Vec<f32> {
    let mut tail = std::mem::take(pending_gap);
    if recorded.is_empty() {
        return Vec::new();
    }
    if tail.len() > OFFLINE_FULL_TAIL_MAX_SAMPLES {
        tail.truncate(OFFLINE_LONG_TAIL_KEEP_SAMPLES);
    }
    tail
}

#[cfg(test)]
mod tests;
