//! Model resolution, loading and transcription for the benchmark harness.
//!
//! ## Where models live
//!
//! The running app resolves catalog models through `ModelManager`, which needs a
//! Tauri `AppHandle` — so the harness deliberately does NOT reuse it. Instead a
//! model is named by an **engine-tagged path** (see [`ModelSpec::parse`]).
//!
//! Note on the "empty models dir": `<app_data>/models/` is empty for the current
//! HuggingFace-sourced GGUF catalog because those models are resolved out of the
//! shared HF hub cache (`~/Library/Caches/huggingface/hub/...` on macOS), not
//! copied into the app data dir (see `ModelManager::get_model_path` /
//! `hf_cached_path`). Only legacy `Url`/`Local` models land in `<app_data>/models`.
//! `--models-dir` therefore defaults to the HF hub cache; pass absolute paths for
//! anything outside it.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

// transcribe-cpp / transcribe-rs are direct dependencies of the crate, so the
// bench module loads engines with the same crates the app uses.
use transcribe_cpp::{Backend, Model, ModelOptions, RunOptions, Session};
use transcribe_rs::{
    onnx::{
        canary::CanaryModel,
        parakeet::{ParakeetModel, ParakeetParams, TimestampGranularity},
        Quantization,
    },
    SpeechModel, TranscribeOptions,
};

/// Which engine a model is loaded through. The harness only supports the
/// engines we currently benchmark: transcribe-cpp GGUF (whisper-family and
/// qwen3-asr), and the two ONNX engines Parakeet and Canary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Any GGUF/GGML file through transcribe-cpp (whisper, qwen3-asr, ...).
    Cpp,
    /// Parakeet ONNX model directory.
    Parakeet,
    /// Canary ONNX model directory (takes a language hint).
    Canary,
}

impl EngineKind {
    fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "cpp" | "whisper" => Some(Self::Cpp),
            "parakeet" => Some(Self::Parakeet),
            "canary" => Some(Self::Canary),
            _ => None,
        }
    }
}

/// A resolved model: a display name, its engine kind and its on-disk path.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    pub name: String,
    pub kind: EngineKind,
    pub path: PathBuf,
}

impl ModelSpec {
    /// Parse a single `--models` entry.
    ///
    /// Grammar: `[<name>=]<spec>` where `<spec>` is either
    /// - `<kind>:<path>` with kind in `cpp` / `whisper` / `parakeet` / `canary`, or
    /// - a bare `<path>`: a `.gguf`/`.bin`/`.ggml` file is inferred as `cpp`; a
    ///   bare directory is ambiguous (Parakeet vs Canary) and must be tagged.
    ///
    /// A relative `<path>` that does not exist as-is is resolved against
    /// `models_dir`. `<name>` defaults to the path's file stem.
    pub fn parse(raw: &str, models_dir: &Path) -> Result<Self> {
        let raw = raw.trim();
        if raw.is_empty() {
            bail!("empty model spec");
        }

        // Optional `name=` prefix: only when the left side is a bare identifier
        // (so `C:\path` or `cpp:/path` are not mistaken for a name).
        let (name_opt, spec) = match raw.split_once('=') {
            Some((lhs, rhs))
                if !lhs.is_empty()
                    && lhs
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) =>
            {
                (Some(lhs.to_string()), rhs)
            }
            _ => (None, raw),
        };

        // Optional `kind:` prefix.
        let (kind_opt, path_str) = match spec.split_once(':') {
            Some((lhs, rhs)) if EngineKind::from_prefix(lhs).is_some() => {
                (EngineKind::from_prefix(lhs), rhs)
            }
            _ => (None, spec),
        };

        let path = Self::resolve_path(path_str.trim(), models_dir);

        let kind = match kind_opt {
            Some(k) => k,
            None => Self::infer_kind(&path)?,
        };

        let name = name_opt.unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or(path_str)
                .to_string()
        });

        Ok(Self { name, kind, path })
    }

    fn resolve_path(path_str: &str, models_dir: &Path) -> PathBuf {
        let p = PathBuf::from(path_str);
        if p.is_absolute() || p.exists() {
            p
        } else {
            models_dir.join(p)
        }
    }

    fn infer_kind(path: &Path) -> Result<EngineKind> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("gguf") | Some("ggml") | Some("bin") => Ok(EngineKind::Cpp),
            _ => bail!(
                "cannot infer engine for {}: prefix the spec with an engine, e.g. \
                 `parakeet:{p}` or `canary:{p}`",
                path.display(),
                p = path.display()
            ),
        }
    }
}

/// A loaded, ready-to-run model. Mirrors the subset of
/// `managers::transcription::LoadedEngine` the harness benchmarks.
pub enum LoadedModel {
    /// transcribe-cpp session plus language/translation capabilities used by the
    /// app run plan. Hints are forwarded only when the arch accepts them and the
    /// loaded model advertises the requested language.
    Cpp {
        session: Session,
        accepts_language_hint: bool,
        languages: Vec<String>,
        supports_translate: bool,
    },
    Parakeet(ParakeetModel),
    Canary(CanaryModel),
}

impl LoadedModel {
    /// Load a model from its spec.
    ///
    /// !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
    /// KEEP IN SYNC WITH `managers::transcription.rs` `LoadedEngine` loading
    /// (the `match model_info.engine_type { ... }` around line 520) and the
    /// per-engine transcription match (around line 1213). This harness loads the
    /// same crates by hand instead of going through `TranscriptionManager`
    /// (which needs a Tauri AppHandle). If the app changes how an engine is
    /// constructed or invoked, update both places.
    /// !!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!!
    pub fn load(spec: &ModelSpec) -> Result<Self> {
        if !spec.path.exists() {
            bail!(
                "model path not found: {} (spec `{}`)",
                spec.path.display(),
                spec.name
            );
        }
        match spec.kind {
            EngineKind::Cpp => {
                // Bench uses the auto backend (Metal on macOS, CPU/Vulkan/CUDA
                // elsewhere) at device 0 — the app's default when no explicit
                // device is selected.
                let options = ModelOptions {
                    backend: Backend::Auto,
                    gpu_device: 0,
                };
                let model = Model::load_with(&spec.path, &options)
                    .map_err(|e| anyhow!("failed to load transcribe-cpp model: {e}"))?;
                let session = model
                    .session()
                    .map_err(|e| anyhow!("failed to create transcribe-cpp session: {e}"))?;
                let model = session.model();
                let caps = model.capabilities();
                let accepts_language_hint =
                    !crate::managers::transcription::arch_rejects_language_hint(&model.arch());
                let languages = caps.languages;
                let supports_translate = caps.supports_translate;
                Ok(Self::Cpp {
                    session,
                    accepts_language_hint,
                    languages,
                    supports_translate,
                })
            }
            EngineKind::Parakeet => {
                let engine = ParakeetModel::load(&spec.path, &Quantization::Int8)
                    .map_err(|e| anyhow!("failed to load parakeet model: {e}"))?;
                Ok(Self::Parakeet(engine))
            }
            EngineKind::Canary => {
                let engine = CanaryModel::load(&spec.path, &Quantization::Int8)
                    .map_err(|e| anyhow!("failed to load canary model: {e}"))?;
                Ok(Self::Canary(engine))
            }
        }
    }

    /// Transcribe 16 kHz mono audio. `language` is a code (e.g. `"de"`) or `None`
    /// for auto/language-agnostic. See the KEEP IN SYNC note on [`Self::load`].
    pub fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<String> {
        match self {
            LoadedModel::Cpp {
                session,
                accepts_language_hint,
                languages,
                supports_translate,
            } => {
                let plan = crate::managers::transcription::transcribe_cpp_run_plan(
                    false,
                    language.unwrap_or("auto"),
                    languages,
                    *supports_translate,
                    *accepts_language_hint,
                );
                let run_options = RunOptions {
                    task: plan.task,
                    language: plan.language,
                    target_language: plan.target_language,
                    ..Default::default()
                };
                session
                    .run(audio, &run_options)
                    .map(|t| t.text)
                    .map_err(|e| anyhow!("transcribe-cpp transcription failed: {e}"))
            }
            LoadedModel::Parakeet(engine) => {
                let params = ParakeetParams {
                    timestamp_granularity: Some(TimestampGranularity::Segment),
                    ..Default::default()
                };
                engine
                    .transcribe_with(audio, &params)
                    .map(|r| r.text)
                    .map_err(|e| anyhow!("parakeet transcription failed: {e}"))
            }
            LoadedModel::Canary(engine) => {
                let options = TranscribeOptions {
                    language: language.map(str::to_string),
                    ..Default::default()
                };
                engine
                    .transcribe(audio, &options)
                    .map(|r| r.text)
                    .map_err(|e| anyhow!("canary transcription failed: {e}"))
            }
        }
    }
}

/// Load a 16 kHz mono `Vec<f32>` from a WAV file, resampling if the file is not
/// already at 16 kHz. Stereo files are downmixed to mono.
pub fn load_wav_16k(path: &Path) -> Result<Vec<f32>> {
    use crate::audio_toolkit::audio::FrameResampler;
    use hound::{SampleFormat, WavReader};
    use std::time::Duration;

    let reader =
        WavReader::open(path).with_context(|| format!("failed to open WAV {}", path.display()))?;
    let spec = reader.spec();
    let channels = spec.channels.max(1) as usize;

    // Read all samples as f32 in -1.0..=1.0, then downmix to mono.
    let interleaved: Vec<f32> = match spec.sample_format {
        SampleFormat::Int => {
            let max = (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader
                .into_samples::<i32>()
                .map(|s| s.map(|v| v as f32 / max))
                .collect::<Result<Vec<f32>, _>>()?
        }
        SampleFormat::Float => reader
            .into_samples::<f32>()
            .collect::<Result<Vec<f32>, _>>()?,
    };

    let mono: Vec<f32> = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / channels as f32)
            .collect()
    };

    const TARGET_HZ: u32 = 16_000;
    if spec.sample_rate == TARGET_HZ {
        return Ok(mono);
    }

    // Reuse the app's frame resampler for the off-rate case.
    let mut rs = FrameResampler::new(
        spec.sample_rate as usize,
        TARGET_HZ as usize,
        Duration::from_millis(30),
    );
    let mut out = Vec::with_capacity(mono.len());
    rs.push(&mono, |frame| out.extend_from_slice(frame));
    rs.finish(|frame| out.extend_from_slice(frame));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_engine_prefixed_specs() {
        let dir = Path::new("/models");
        let s = ModelSpec::parse("parakeet:/abs/parakeet-v2", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Parakeet);
        assert_eq!(s.path, PathBuf::from("/abs/parakeet-v2"));
        assert_eq!(s.name, "parakeet-v2");

        let s = ModelSpec::parse("canary:/abs/canary-dir", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Canary);

        let s = ModelSpec::parse("whisper:/abs/ggml-small.bin", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Cpp);
        assert_eq!(s.name, "ggml-small");
    }

    #[test]
    fn parse_named_and_inferred() {
        let dir = Path::new("/models");
        let s = ModelSpec::parse("turbo=cpp:/abs/turbo.gguf", dir).unwrap();
        assert_eq!(s.name, "turbo");
        assert_eq!(s.kind, EngineKind::Cpp);

        // Bare file path infers cpp from extension.
        let s = ModelSpec::parse("/abs/model.gguf", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Cpp);
    }

    #[test]
    fn relative_path_resolved_against_models_dir() {
        let dir = Path::new("/models");
        let s = ModelSpec::parse("cpp:sub/model.gguf", dir).unwrap();
        assert_eq!(s.path, PathBuf::from("/models/sub/model.gguf"));
    }

    #[test]
    fn bare_directory_is_ambiguous() {
        let dir = Path::new("/models");
        let err = ModelSpec::parse("/abs/some-onnx-dir", dir).unwrap_err();
        assert!(err.to_string().contains("cannot infer engine"));
    }
}
