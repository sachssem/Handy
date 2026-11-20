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

// transcribe-cpp is a direct dependency of the crate, so the bench loads
// models with the same crate the app's transcription worker uses. It runs
// in-process: the app's worker re-launches the Handy executable, which this
// binary is not.
use transcribe_cpp::{Backend, Feature, Model, ModelOptions, RunOptions, Session};

/// Which engine a model is loaded through. Every model runs through
/// transcribe-cpp now (GGUF: whisper, parakeet, canary, qwen3-asr, ...); the
/// old ONNX engine prefixes are kept as aliases so existing specs still parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineKind {
    /// Any GGUF/GGML file through transcribe-cpp.
    Cpp,
}

impl EngineKind {
    fn from_prefix(s: &str) -> Option<Self> {
        match s {
            "cpp" | "whisper" | "parakeet" | "canary" => Some(Self::Cpp),
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
    /// - `<kind>:<path>` with kind in `cpp` / `whisper` / `parakeet` / `canary`
    ///   (all transcribe-cpp GGUF), or
    /// - a bare `<path>`: a `.gguf`/`.bin`/`.ggml` file is inferred as `cpp`.
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
                "cannot infer engine for {}: expected a .gguf/.ggml/.bin file, or \
                 prefix the spec with an engine, e.g. `cpp:{p}`",
                path.display(),
                p = path.display()
            ),
        }
    }
}

/// A loaded, ready-to-run transcribe-cpp model with the capabilities the
/// app's run setup reads.
pub struct LoadedModel {
    session: Session,
    languages: Vec<String>,
    supports_translate: bool,
    supports_vocabulary: bool,
    /// Context prompt sent with every run (`--prompt`), to replay a dictation.
    prompt: Option<String>,
}

/// One bench transcription: the text and whether the custom words went to the
/// model as vocabulary (the app then skips fuzzy correction).
pub struct BenchTranscript {
    pub text: String,
    pub vocabulary_sent: bool,
}

impl LoadedModel {
    /// Load a model from its spec.
    ///
    /// KEEP IN SYNC WITH `managers::transcription::run_setup` (task, language
    /// and vocabulary of a run). The harness loads transcribe-cpp by hand
    /// instead of going through `TranscriptionManager` (which needs a Tauri
    /// AppHandle and a worker process).
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
                // elsewhere) with automatic device selection — the app's default
                // when no explicit device is selected.
                let options = ModelOptions {
                    backend: Backend::Auto,
                    device: None,
                    ..Default::default()
                };
                let model = Model::load_with(&spec.path, &options)
                    .map_err(|e| anyhow!("failed to load transcribe-cpp model: {e}"))?;
                let session = model
                    .session()
                    .map_err(|e| anyhow!("failed to create transcribe-cpp session: {e}"))?;
                let model = session.model();
                let caps = model
                    .capabilities()
                    .map_err(|e| anyhow!("failed to read model capabilities: {e}"))?;
                let supports_vocabulary = model.supports(Feature::Vocabulary);
                Ok(Self {
                    session,
                    languages: caps.languages,
                    supports_translate: caps.supports_translate,
                    supports_vocabulary,
                    prompt: None,
                })
            }
        }
    }

    /// Send `prompt` as the context prompt of every following run, as the
    /// app's ASR biasing does with the text before the caret.
    pub fn set_prompt(&mut self, prompt: Option<String>) {
        self.prompt = prompt;
    }

    /// Run the app's post-load warm-up inference (silence, default options).
    /// The first real run after a load otherwise differs from the app's.
    pub fn warm_up(&mut self) {
        let session = &mut self.session;
        crate::model_warmup::run(|silence| session.run(&silence, &RunOptions::default()));
    }

    /// Transcribe 16 kHz mono audio. `language` is a code (e.g. `"de"`) or `None`
    /// for auto/language-agnostic. Custom words go to the model as vocabulary
    /// where it takes them, as in the app. See the KEEP IN SYNC note on
    /// [`Self::load`].
    pub fn transcribe(
        &mut self,
        audio: &[f32],
        language: Option<&str>,
        custom_words: &[String],
    ) -> Result<BenchTranscript> {
        let plan = crate::managers::transcription::transcribe_cpp_run_plan(
            false,
            language.unwrap_or("auto"),
            &self.languages,
            self.supports_translate,
        );
        let vocabulary = if self.supports_vocabulary {
            custom_words.to_vec()
        } else {
            Vec::new()
        };
        let run_options = RunOptions {
            task: plan.task,
            language: plan.language,
            target_language: plan.target_language,
            vocabulary,
            prompt: self.prompt.clone(),
            ..Default::default()
        };
        let vocabulary_sent = !run_options.vocabulary.is_empty();
        self.session
            .run(audio, &run_options)
            .map(|t| BenchTranscript {
                text: t.text,
                vocabulary_sent,
            })
            .map_err(|e| anyhow!("transcribe-cpp transcription failed: {e}"))
    }
}

/// Load a 16 kHz mono `Vec<f32>` from a WAV file, resampling if the file is not
/// already at 16 kHz. Stereo files are downmixed to mono. Uses the app's file
/// decoder (the one file transcription uses).
pub fn load_wav_16k(path: &Path) -> Result<Vec<f32>> {
    crate::audio_toolkit::decode_audio_file(path)
        .with_context(|| format!("failed to read audio {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn parse_engine_prefixed_specs() {
        let dir = Path::new("/models");
        // The old ONNX prefixes are aliases for transcribe-cpp now.
        let s = ModelSpec::parse("parakeet:/abs/parakeet-v2.gguf", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Cpp);
        assert_eq!(s.path, PathBuf::from("/abs/parakeet-v2.gguf"));
        assert_eq!(s.name, "parakeet-v2");

        let s = ModelSpec::parse("canary:/abs/canary.gguf", dir).unwrap();
        assert_eq!(s.kind, EngineKind::Cpp);

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
    fn bare_directory_is_not_a_model() {
        let dir = Path::new("/models");
        let err = ModelSpec::parse("/abs/some-onnx-dir", dir).unwrap_err();
        assert!(err.to_string().contains("cannot infer engine"));
    }
}
