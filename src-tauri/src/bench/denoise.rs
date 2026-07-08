//! Optional DTLN speech-enhancement (denoise) stage for the benchmark harness
//! (fork feature: voice-control).
//!
//! DTLN (<https://github.com/breizhn/DTLN>, MIT) is a two-stage real-time speech
//! enhancer that operates natively at 16 kHz — the exact rate the harness feeds
//! the ASR engines — so it slots in directly before [`super::engine`]'s
//! transcription call with no resampling.
//!
//! It runs two small stateful ONNX models per 8 ms hop over a 32 ms / 512-sample
//! block (128-sample shift, 4× overlap):
//!
//! 1. `model_1` masks the magnitude spectrum (`in_mag * mask`, original phase
//!    kept), then an inverse rFFT reconstructs a time-domain block;
//! 2. `model_2` is a learned time-domain filter over that block.
//!
//! Both models carry LSTM state (`[1, 2, 128, 2]`) across hops; the state is
//! zero-initialised per utterance. This mirrors the reference
//! `real_time_dtln_audio.py` exactly (rectangular analysis window, overlap-add
//! synthesis, no synthesis window).
//!
//! Speech enhancement frequently *hurts* modern ASR (it introduces artefacts the
//! acoustic model was never trained on), so the CLI also exposes mix-back
//! configs: `out = enhanced * a + raw * (1 - a)`. See `--denoise` in
//! [`super`] and `bench/README.md`.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use ort::session::{builder::GraphOptimizationLevel, Session};
use ort::value::Tensor;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// DTLN block length in samples (32 ms at 16 kHz).
const BLOCK_LEN: usize = 512;
/// DTLN block shift / hop in samples (8 ms at 16 kHz).
const BLOCK_SHIFT: usize = 128;
/// Real-FFT bins for a 512-point block (`BLOCK_LEN / 2 + 1`).
const N_BINS: usize = BLOCK_LEN / 2 + 1;
/// Flattened LSTM state length per model (`1 * 2 * 128 * 2`).
const STATE_LEN: usize = 2 * 128 * 2;

/// The two pretrained DTLN ONNX models, pinned by sha256 (verified on first
/// download and on every reuse). Sourced from the upstream repo's
/// `pretrained_model/` directory.
struct ModelFile {
    name: &'static str,
    url: &'static str,
    sha256: &'static str,
}

const MODELS: [ModelFile; 2] = [
    ModelFile {
        name: "model_1.onnx",
        url: "https://github.com/breizhn/DTLN/raw/master/pretrained_model/model_1.onnx",
        sha256: "22b91cae3855e5a0620e66a917ca6c82c58db0e842c770f58d86751c5e8d4ae3",
    },
    ModelFile {
        name: "model_2.onnx",
        url: "https://github.com/breizhn/DTLN/raw/master/pretrained_model/model_2.onnx",
        sha256: "e20c92f9233fccf29cddf86970d0d0161a03aebccc26d6f4d5639c4d5ec2e639",
    },
];

/// The `--denoise` CLI choice. `mixNN = enhanced * 0.NN + raw * (1 - 0.NN)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DenoiseMode {
    /// No denoising — the raw 16 kHz audio reaches the engine unchanged.
    None,
    /// Full DTLN enhancement (`enhanced` only).
    Dtln,
    /// 70% DTLN, 30% raw mix-back.
    #[value(name = "dtln-mix70")]
    DtlnMix70,
    /// 50% DTLN, 50% raw mix-back.
    #[value(name = "dtln-mix50")]
    DtlnMix50,
}

impl DenoiseMode {
    /// The enhanced-signal weight `a`, or `None` when denoising is disabled.
    fn mix(self) -> Option<f32> {
        match self {
            DenoiseMode::None => Option::None,
            DenoiseMode::Dtln => Some(1.0),
            DenoiseMode::DtlnMix70 => Some(0.7),
            DenoiseMode::DtlnMix50 => Some(0.5),
        }
    }

    /// Stable label recorded in the run report metadata.
    pub fn label(self) -> &'static str {
        match self {
            DenoiseMode::None => "none",
            DenoiseMode::Dtln => "dtln",
            DenoiseMode::DtlnMix70 => "dtln-mix70",
            DenoiseMode::DtlnMix50 => "dtln-mix50",
        }
    }
}

/// A constructed denoise stage. Built once per run and reused across all
/// utterances; DTLN's LSTM state is reset inside each [`Self::process`] call so
/// takes never leak state into one another.
pub enum Denoiser {
    /// Pass-through (`--denoise none`).
    Off,
    /// DTLN with the given enhanced-signal mix weight `a`.
    Dtln { model: Dtln, mix: f32 },
}

impl Denoiser {
    /// Build the denoiser for `mode`, loading (and, if needed, downloading) the
    /// DTLN models for the enhancing modes.
    pub fn new(mode: DenoiseMode) -> Result<Self> {
        match mode.mix() {
            None => Ok(Denoiser::Off),
            Some(mix) => Ok(Denoiser::Dtln {
                model: Dtln::load()?,
                mix,
            }),
        }
    }

    /// Apply the stage to one utterance of 16 kHz mono samples.
    pub fn process(&mut self, audio: &[f32]) -> Result<Vec<f32>> {
        match self {
            Denoiser::Off => Ok(audio.to_vec()),
            Denoiser::Dtln { model, mix } => {
                let enhanced = model.process(audio)?;
                Ok(mix_back(audio, &enhanced, *mix))
            }
        }
    }
}

/// `out[i] = enhanced[i] * a + raw[i] * (1 - a)`, elementwise over the shared
/// length (both slices are the same length in practice).
fn mix_back(raw: &[f32], enhanced: &[f32], a: f32) -> Vec<f32> {
    raw.iter()
        .zip(enhanced)
        .map(|(r, e)| e * a + r * (1.0 - a))
        .collect()
}

/// A loaded DTLN model pair plus its forward/inverse real-FFT plans.
pub struct Dtln {
    model1: Session,
    model2: Session,
    fft: Arc<dyn RealToComplex<f32>>,
    ifft: Arc<dyn ComplexToReal<f32>>,
}

impl Dtln {
    /// Load both ONNX models (downloading + sha256-verifying them on first use)
    /// and build the FFT plans.
    pub fn load() -> Result<Self> {
        let dir = ensure_models()?;
        let build = |name: &str| -> Result<Session> {
            let session = Session::builder()
                .map_err(|e| anyhow!("failed to load DTLN {name}: {e}"))?
                .with_optimization_level(GraphOptimizationLevel::Level3)
                .map_err(|e| anyhow!("failed to load DTLN {name}: {e}"))?
                .commit_from_file(dir.join(name))
                .map_err(|e| anyhow!("failed to load DTLN {name}: {e}"))?;
            Ok(session)
        };
        let model1 = build("model_1.onnx")?;
        let model2 = build("model_2.onnx")?;

        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(BLOCK_LEN);
        let ifft = planner.plan_fft_inverse(BLOCK_LEN);

        Ok(Self {
            model1,
            model2,
            fft,
            ifft,
        })
    }

    /// Enhance one utterance. Returns a buffer the same length as `audio`; the
    /// trailing `BLOCK_LEN - BLOCK_SHIFT` samples that never fill a full hop stay
    /// silent, matching the reference implementation.
    pub fn process(&mut self, audio: &[f32]) -> Result<Vec<f32>> {
        // Too short to form a single block: nothing to enhance, pass through.
        if audio.len() < BLOCK_LEN {
            return Ok(audio.to_vec());
        }

        let mut in_buffer = [0f32; BLOCK_LEN];
        let mut out_buffer = [0f32; BLOCK_LEN];
        let mut states1 = vec![0f32; STATE_LEN];
        let mut states2 = vec![0f32; STATE_LEN];
        let mut out = vec![0f32; audio.len()];

        let mut fft_in = self.fft.make_input_vec();
        let mut spectrum = self.fft.make_output_vec();
        let mut ifft_out = self.ifft.make_output_vec();
        let inv_scale = 1.0 / BLOCK_LEN as f32; // realfft inverse is unnormalised

        let num_blocks = (audio.len() - (BLOCK_LEN - BLOCK_SHIFT)) / BLOCK_SHIFT;
        for idx in 0..num_blocks {
            let start = idx * BLOCK_SHIFT;

            // Slide the analysis buffer and append the next hop of input.
            in_buffer.copy_within(BLOCK_SHIFT.., 0);
            in_buffer[BLOCK_LEN - BLOCK_SHIFT..]
                .copy_from_slice(&audio[start..start + BLOCK_SHIFT]);

            // rFFT of the (rectangular-windowed) block.
            fft_in.copy_from_slice(&in_buffer);
            self.fft
                .process(&mut fft_in, &mut spectrum)
                .map_err(|e| anyhow!("DTLN forward FFT failed: {e}"))?;

            // model_1: magnitude mask (original phase kept).
            let mag: Vec<f32> = spectrum.iter().map(|c| c.norm()).collect();
            let mask = self.run_model1(&mag, &mut states1)?;
            for (c, m) in spectrum.iter_mut().zip(&mask) {
                *c *= *m;
            }

            // Back to the time domain (masked magnitude, original phase).
            self.ifft
                .process(&mut spectrum, &mut ifft_out)
                .map_err(|e| anyhow!("DTLN inverse FFT failed: {e}"))?;
            let estimated: Vec<f32> = ifft_out.iter().map(|v| v * inv_scale).collect();

            // model_2: learned time-domain filter.
            let enhanced_block = self.run_model2(&estimated, &mut states2)?;

            // Overlap-add synthesis; emit the leading hop.
            out_buffer.copy_within(BLOCK_SHIFT.., 0);
            out_buffer[BLOCK_LEN - BLOCK_SHIFT..].fill(0.0);
            for (o, b) in out_buffer.iter_mut().zip(&enhanced_block) {
                *o += *b;
            }
            out[start..start + BLOCK_SHIFT].copy_from_slice(&out_buffer[..BLOCK_SHIFT]);
        }

        Ok(out)
    }

    /// Run `model_1` for one block: returns the `N_BINS` magnitude mask and
    /// updates `states` in place with the model's next LSTM state.
    fn run_model1(&mut self, mag: &[f32], states: &mut Vec<f32>) -> Result<Vec<f32>> {
        let mag_t = Tensor::from_array((vec![1i64, 1, N_BINS as i64], mag.to_vec()))
            .map_err(|e| anyhow!("DTLN model_1 input tensor: {e}"))?;
        let st_t = Tensor::from_array((vec![1i64, 2, 128, 2], states.clone()))
            .map_err(|e| anyhow!("DTLN model_1 state tensor: {e}"))?;
        let outputs = self
            .model1
            .run(ort::inputs!["input_2" => mag_t, "input_3" => st_t])
            .map_err(|e| anyhow!("DTLN model_1 inference: {e}"))?;
        let mask = extract_f32(&outputs, "activation_2")?;
        *states = extract_f32(&outputs, "tf_op_layer_stack_2")?;
        Ok(mask)
    }

    /// Run `model_2` for one block: returns the enhanced `BLOCK_LEN`-sample
    /// time-domain block and updates `states` in place.
    fn run_model2(&mut self, block: &[f32], states: &mut Vec<f32>) -> Result<Vec<f32>> {
        let block_t = Tensor::from_array((vec![1i64, 1, BLOCK_LEN as i64], block.to_vec()))
            .map_err(|e| anyhow!("DTLN model_2 input tensor: {e}"))?;
        let st_t = Tensor::from_array((vec![1i64, 2, 128, 2], states.clone()))
            .map_err(|e| anyhow!("DTLN model_2 state tensor: {e}"))?;
        let outputs = self
            .model2
            .run(ort::inputs!["input_4" => block_t, "input_5" => st_t])
            .map_err(|e| anyhow!("DTLN model_2 inference: {e}"))?;
        let block = extract_f32(&outputs, "conv1d_3")?;
        *states = extract_f32(&outputs, "tf_op_layer_stack_5")?;
        Ok(block)
    }
}

/// Extract a named ONNX output as a flat, C-order `Vec<f32>`.
fn extract_f32(outputs: &ort::session::SessionOutputs, name: &str) -> Result<Vec<f32>> {
    let arr = outputs[name]
        .try_extract_array::<f32>()
        .map_err(|e| anyhow!("DTLN output `{name}` extract: {e}"))?;
    Ok(arr.iter().copied().collect())
}

/// Ensure both DTLN models exist and match their pinned sha256, downloading any
/// that are missing. Returns the model directory.
fn ensure_models() -> Result<PathBuf> {
    let dir = super::cache_dir().join("handy-bench-models").join("dtln");
    for m in &MODELS {
        let path = dir.join(m.name);
        if path.exists() {
            verify_sha256(&path, m.sha256)
                .with_context(|| format!("cached DTLN model {} failed verification", m.name))?;
        } else {
            download_model(&dir, m)?;
        }
    }
    Ok(dir)
}

/// Download one model to `dir`, verifying its sha256 before writing it to the
/// final path.
fn download_model(dir: &Path, m: &ModelFile) -> Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("failed to create DTLN model dir {}", dir.display()))?;
    eprintln!("Downloading DTLN {} from {} ...", m.name, m.url);
    let bytes = reqwest::blocking::get(m.url)
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.bytes())
        .with_context(|| format!("failed to download DTLN model {}", m.name))?;
    let got = sha256_hex(&bytes);
    if got != m.sha256 {
        bail!(
            "DTLN model {} sha256 mismatch: expected {}, got {}",
            m.name,
            m.sha256,
            got
        );
    }
    std::fs::write(dir.join(m.name), &bytes)
        .with_context(|| format!("failed to write DTLN model {}", m.name))?;
    Ok(())
}

/// Verify an on-disk file against a hex sha256.
fn verify_sha256(path: &Path, expected: &str) -> Result<()> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read {} for hashing", path.display()))?;
    let got = sha256_hex(&bytes);
    if got != expected {
        bail!("sha256 mismatch: expected {expected}, got {got}");
    }
    Ok(())
}

/// Lower-case hex sha256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(digest.len() * 2);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Forward rFFT then normalised inverse rFFT round-trips a signal within a
    /// tight tolerance — the DSP backbone of the STFT/iSTFT stage.
    #[test]
    fn stft_roundtrip_identity() {
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(BLOCK_LEN);
        let ifft = planner.plan_fft_inverse(BLOCK_LEN);

        let mut input: Vec<f32> = (0..BLOCK_LEN)
            .map(|n| {
                let t = n as f32 / BLOCK_LEN as f32;
                0.5 * (2.0 * std::f32::consts::PI * 7.0 * t).sin()
                    + 0.3 * (2.0 * std::f32::consts::PI * 31.0 * t).cos()
            })
            .collect();
        let original = input.clone();

        let mut spectrum = fft.make_output_vec();
        fft.process(&mut input, &mut spectrum).unwrap();

        let mut output = ifft.make_output_vec();
        ifft.process(&mut spectrum, &mut output).unwrap();
        let inv_scale = 1.0 / BLOCK_LEN as f32;

        for (a, b) in original.iter().zip(&output) {
            assert!(
                (a - b * inv_scale).abs() < 1e-4,
                "roundtrip drift {a} vs {b}"
            );
        }
    }

    /// Mix-back endpoints and an interior weight behave as specified.
    #[test]
    fn mix_back_math() {
        let raw = [1.0f32, 2.0, 3.0, 4.0];
        let enhanced = [0.0f32, 0.0, 0.0, 0.0];

        // a = 1.0 -> pure enhanced.
        assert_eq!(mix_back(&raw, &enhanced, 1.0), enhanced.to_vec());
        // a = 0.0 -> pure raw.
        assert_eq!(mix_back(&raw, &enhanced, 0.0), raw.to_vec());
        // a = 0.7 -> weighted blend (enhanced is zero, so 0.3 * raw).
        let mixed = mix_back(&raw, &enhanced, 0.7);
        for (m, r) in mixed.iter().zip(&raw) {
            assert!((m - 0.3 * r).abs() < 1e-6);
        }
    }

    /// Mapping from CLI mode to mix weight / label is stable.
    #[test]
    fn mode_weights_and_labels() {
        assert_eq!(DenoiseMode::None.mix(), None);
        assert_eq!(DenoiseMode::Dtln.mix(), Some(1.0));
        assert_eq!(DenoiseMode::DtlnMix70.mix(), Some(0.7));
        assert_eq!(DenoiseMode::DtlnMix50.mix(), Some(0.5));
        assert_eq!(DenoiseMode::DtlnMix50.label(), "dtln-mix50");
    }

    /// Smoke test: when the DTLN models are already cached locally, processing is
    /// deterministic and state-safe — identical input yields identical output on
    /// repeated calls (proving per-utterance state reset), the output length is
    /// preserved, and every sample is finite. Skipped when the models are absent
    /// (no network in CI).
    #[test]
    fn state_continuity_smoke() {
        let dir = super::super::cache_dir()
            .join("handy-bench-models")
            .join("dtln");
        if !dir.join("model_1.onnx").exists() || !dir.join("model_2.onnx").exists() {
            eprintln!("DTLN models not cached; skipping state_continuity_smoke");
            return;
        }
        let mut dtln = Dtln::load().expect("load DTLN");
        let audio: Vec<f32> = (0..8000)
            .map(|n| 0.1 * (2.0 * std::f32::consts::PI * 220.0 * n as f32 / 16_000.0).sin())
            .collect();
        let a = dtln.process(&audio).expect("process 1");
        let b = dtln.process(&audio).expect("process 2");
        assert_eq!(a.len(), audio.len());
        assert_eq!(a, b, "state must reset per utterance");
        assert!(a.iter().all(|v| v.is_finite()));
    }
}
