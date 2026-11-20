//! fork(voice-control): vad-edges regressions, ported from the fork's old
//! `recorder/tests.rs` onto the capture sink. They drive the real smoothing
//! wrapper (resumed pre-roll overlap, the final zero-padded chunk) through
//! `CaptureSink::process_chunk`.

use super::take_tail;
use crate::audio_toolkit::audio::sink::{
    AudioFrameCallback, CaptureSink, CaptureVad, LevelCallback, VadPolicy,
};
use crate::audio_toolkit::vad::{SmoothedVad, VadFrame, VoiceActivityDetector};
use handy_recorder::{AudioChunk, Sink};
use std::sync::{mpsc, Arc, Mutex};

const FRAME: usize = 480;

struct ScriptedRecordingVad {
    script: Vec<bool>,
    next_frame: usize,
}

impl VoiceActivityDetector for ScriptedRecordingVad {
    fn push_frame<'a>(&'a mut self, frame: &'a [f32]) -> anyhow::Result<VadFrame<'a>> {
        let voiced = self.script.get(self.next_frame).copied().unwrap_or(false);
        self.next_frame += 1;
        Ok(if voiced {
            VadFrame::Speech(frame)
        } else {
            VadFrame::Noise
        })
    }

    fn frame_samples(&self) -> usize {
        FRAME
    }

    fn reset(&mut self) {
        self.next_frame = 0;
    }
}

struct Harness {
    sink: CaptureSink,
    fed: Arc<Mutex<Vec<f32>>>,
}

fn sink_with(detector: Box<dyn VoiceActivityDetector>, policy: VadPolicy) -> Harness {
    let fed = Arc::new(Mutex::new(Vec::new()));
    let on_audio: AudioFrameCallback = {
        let fed = Arc::clone(&fed);
        Arc::new(move |frame: &[f32]| fed.lock().unwrap().extend_from_slice(frame))
    };
    let on_levels: LevelCallback = Arc::new(|_| {});
    let (ready_tx, _ready) = mpsc::channel();
    Harness {
        sink: CaptureSink::new(
            CaptureVad::new(detector, 2, 2),
            policy,
            on_levels,
            on_audio,
            ready_tx,
        ),
        fed,
    }
}

/// The real smoothing wrapper (prefill 3, hangover 2, onset 2) over a script.
fn smoothed(script: &[bool], policy: VadPolicy) -> Harness {
    let detector = SmoothedVad::new(
        Box::new(ScriptedRecordingVad {
            script: script.to_vec(),
            next_frame: 0,
        }),
        3,
        2,
        2,
    );
    sink_with(Box::new(detector), policy)
}

fn numbered_frames(count: usize) -> Vec<f32> {
    (0..count)
        .flat_map(|index| std::iter::repeat_n((index + 1) as f32, FRAME))
        .collect()
}

fn chunk(samples: &[f32], valid_frames: usize) -> AudioChunk<'_> {
    AudioChunk {
        samples,
        sample_rate: 16_000,
        channels: 1,
        valid_frames,
    }
}

/// Feed `input` frame by frame; a short last frame goes in zero-padded, as
/// the recorder delivers its final chunk.
fn feed(sink: &mut CaptureSink, input: &[f32]) {
    for frame in input.chunks(FRAME) {
        if frame.len() == FRAME {
            sink.process_chunk(chunk(frame, FRAME));
        } else {
            let mut padded = frame.to_vec();
            padded.resize(FRAME, 0.0);
            sink.process_chunk(chunk(&padded, frame.len()));
        }
    }
}

fn capture_script(script: &[bool], policy: VadPolicy) -> Vec<f32> {
    let mut h = smoothed(script, policy);
    feed(&mut h.sink, &numbered_frames(script.len()));
    h.sink.finish().samples
}

#[test]
fn offline_dictation_keeps_long_internal_gap_without_duplicate_preroll() {
    let mut script = vec![false; 5];
    script.extend([true; 2]);
    script.extend([false; 20]); // 600 ms: exceeds both pre-roll and hangover.
    script.extend([true; 2]);
    script.extend([false; 5]);

    let samples = capture_script(&script, VadPolicy::Offline);
    let input = numbered_frames(script.len());
    // Initial pre-roll begins at frame 3; the short trailing gap stays whole.
    assert_eq!(samples, input[3 * FRAME..]);
}

/// Sample-exact pause regressions: an unsmoothed detector and a gap fed as
/// one chunk, so 2 s / 10 s are not rounded to 30 ms frames.
fn capture_internal_gap(gap: &[f32]) -> Vec<f32> {
    let detector = ScriptedRecordingVad {
        script: vec![true, false, true],
        next_frame: 0,
    };
    let mut h = sink_with(Box::new(detector), VadPolicy::Offline);
    for frame in [&[1.0; FRAME][..], gap, &[2.0; FRAME][..]] {
        h.sink.process_chunk(chunk(frame, frame.len()));
    }
    assert!(h.sink.pending_gap.is_empty());
    h.sink.finish().samples
}

#[test]
fn offline_dictation_keeps_two_second_internal_gap_whole() {
    let gap: Vec<f32> = (0..32_000).map(|sample| sample as f32).collect();
    let expected = [&[1.0; FRAME][..], &gap, &[2.0; FRAME][..]].concat();
    assert_eq!(capture_internal_gap(&gap), expected);
}

#[test]
fn offline_dictation_keeps_three_second_internal_gap_whole() {
    let gap = vec![0.0; 48_000];
    let expected = [&[1.0; FRAME][..], &gap, &[2.0; FRAME][..]].concat();
    assert_eq!(capture_internal_gap(&gap), expected);
}

#[test]
fn offline_dictation_caps_ten_second_internal_gap_at_one_point_five_seconds() {
    let gap: Vec<f32> = (0..160_000).map(|sample| sample as f32).collect();
    let expected = [
        &[1.0; FRAME][..],
        &gap[..12_000],
        &gap[148_000..],
        &[2.0; FRAME][..],
    ]
    .concat();
    assert_eq!(capture_internal_gap(&gap), expected);
}

#[test]
fn offline_dictation_trims_leading_silence_and_keeps_short_tail() {
    let script = [
        false, false, false, false, false, true, true, false, false, false, false,
    ];
    let input = numbered_frames(script.len());
    assert_eq!(
        capture_script(&script, VadPolicy::Offline),
        input[3 * FRAME..]
    );
}

#[test]
fn offline_dictation_without_confirmed_speech_stays_empty() {
    assert!(capture_script(&[false; 20], VadPolicy::Offline).is_empty());
    assert!(capture_script(&[false, false, true], VadPolicy::Offline).is_empty());
}

#[test]
fn offline_dictation_short_gap_does_not_repeat_previous_hangover() {
    let script = [
        true, true, false, false, false, true, true, false, false, false,
    ];
    let input = numbered_frames(script.len());
    assert_eq!(capture_script(&script, VadPolicy::Offline), input);
}

#[test]
fn offline_dictation_final_padded_chunk_can_confirm_resumed_speech() {
    let mut script = vec![true; 2];
    script.extend([false; 12]);
    script.extend([true; 2]);
    let mut input = numbered_frames(script.len());
    input.truncate(input.len() - 240);
    let mut h = smoothed(&script, VadPolicy::Offline);
    feed(&mut h.sink, &input[..(script.len() - 1) * FRAME]);
    assert_eq!(h.sink.samples.len(), 4 * FRAME);
    feed(&mut h.sink, &input[(script.len() - 1) * FRAME..]);
    input.resize(script.len() * FRAME, 0.0);
    assert!(h.sink.pending_gap.is_empty());
    assert_eq!(h.sink.finish().samples, input);
}

// Stop-time tail retention protects quiet final words without keeping
// arbitrarily long trailing silence or losing the final chunk.
#[test]
fn offline_dictation_keeps_quiet_final_word_after_hangover() {
    let script = [true, true, false, false, false, false, false];
    let mut input = numbered_frames(script.len());
    // The final word has energy, but the detector labels it noise after hangover.
    input[4 * FRAME..].fill(0.005);
    let mut h = smoothed(&script, VadPolicy::Offline);
    feed(&mut h.sink, &input);
    assert_eq!(h.sink.samples.len(), 4 * FRAME);
    assert_eq!(h.sink.pending_gap, input[4 * FRAME..]);
    assert_eq!(h.sink.finish().samples, input);
}

#[test]
fn offline_dictation_keeps_trailing_gap_at_one_point_five_seconds() {
    let mut script = vec![true; 2];
    script.extend([false; 52]); // Two hangover frames, then exactly 1500 ms.
    assert_eq!(
        capture_script(&script, VadPolicy::Offline),
        numbered_frames(script.len())
    );
}

#[test]
fn offline_dictation_caps_long_trailing_silence_at_seven_hundred_ms() {
    let mut script = vec![true; 2];
    script.extend([false; 100]);
    let input = numbered_frames(script.len());
    // Keep speech + hangover, then precisely 700 ms of the pending gap.
    assert_eq!(
        capture_script(&script, VadPolicy::Offline),
        input[..4 * FRAME + 11_200]
    );
}

#[test]
fn offline_dictation_keeps_noise_tail_from_final_padded_chunk() {
    let script = [true, true, false, false, false, false];
    let mut input = numbered_frames(script.len());
    input.truncate(input.len() - 240);
    let mut h = smoothed(&script, VadPolicy::Offline);
    feed(&mut h.sink, &input[..(script.len() - 1) * FRAME]);
    assert_eq!(h.sink.pending_gap.len(), FRAME);
    // The final, padded chunk is held as a whole frame like any other.
    feed(&mut h.sink, &input[(script.len() - 1) * FRAME..]);
    assert_eq!(h.sink.pending_gap.len(), 2 * FRAME);
    input.resize(script.len() * FRAME, 0.0);
    assert_eq!(h.sink.finish().samples, input);
}

#[test]
fn offline_dictation_tail_cap_applies_after_classifying_the_last_chunk() {
    let mut script = vec![true; 2];
    script.extend([false; 53]);
    let mut input = numbered_frames(script.len());
    input.truncate(input.len() - 240);
    let mut h = smoothed(&script, VadPolicy::Offline);
    feed(&mut h.sink, &input[..(script.len() - 1) * FRAME]);
    assert_eq!(h.sink.pending_gap.len(), 24_000); // Exactly 1500 ms before the last chunk.
    feed(&mut h.sink, &input[(script.len() - 1) * FRAME..]);
    assert_eq!(h.sink.finish().samples, input[..4 * FRAME + 11_200]);
}

#[test]
fn offline_dictation_pending_tail_does_not_leak_into_next_recording() {
    let mut h = smoothed(&[true, true, false, false, false], VadPolicy::Offline);
    feed(&mut h.sink, &numbered_frames(5));
    assert_eq!(h.sink.pending_gap.len(), FRAME);
    let captured = h.sink.finish();
    assert_eq!(captured.samples, numbered_frames(5));

    // The next recording gets a fresh sink around the same detector.
    let (ready_tx, _ready) = mpsc::channel();
    let mut next = CaptureSink::new(
        captured.vad,
        VadPolicy::Offline,
        Arc::new(|_| {}),
        Arc::new(|_: &[f32]| {}),
        ready_tx,
    );
    assert!(next.pending_gap.is_empty());
    feed(&mut next, &numbered_frames(2));
    assert_eq!(next.finish().samples, numbered_frames(2));
}

#[test]
fn take_tail_releases_the_gap_allocation() {
    let mut gap = vec![0.5; 30_000];
    let tail = take_tail(&[1.0], &mut gap);
    assert_eq!(tail.len(), 11_200);
    assert_eq!(gap.capacity(), 0);
    let mut gap = vec![0.5; 10];
    assert!(take_tail(&[], &mut gap).is_empty());
    assert_eq!(gap.capacity(), 0);
}

// The live feed (long-form chunks decoded while recording) gets exactly the
// recording, so its sample count matches the recording's at finalize.
#[test]
fn offline_live_feed_matches_the_recording() {
    let mut script = vec![false; 5];
    script.extend([true; 2]);
    script.extend([false; 20]);
    script.extend([true; 2]);
    script.extend([false; 60]);
    let mut h = smoothed(&script, VadPolicy::Offline);
    feed(&mut h.sink, &numbered_frames(script.len()));
    let fed = Arc::clone(&h.fed);
    let samples = h.sink.finish().samples;
    assert_eq!(*fed.lock().unwrap(), samples);
}

#[test]
fn streaming_dictation_keeps_existing_gap_filtering_and_callbacks() {
    let mut script = vec![false; 5];
    script.extend([true; 2]);
    script.extend([false; 20]);
    script.extend([true; 2]);
    script.extend([false; 5]);
    let mut h = smoothed(&script, VadPolicy::Streaming);
    let input = numbered_frames(script.len());
    feed(&mut h.sink, &input);
    let expected = [&input[3 * FRAME..9 * FRAME], &input[25 * FRAME..31 * FRAME]].concat();
    assert!(h.sink.pending_gap.is_empty());
    let fed = Arc::clone(&h.fed);
    assert_eq!(h.sink.finish().samples, expected);
    assert_eq!(*fed.lock().unwrap(), expected);
}
