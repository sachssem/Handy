//! Headless voice-dictation benchmark harness (fork feature: voice-control).
//!
//! Runs a corpus of the user's own voice samples (WAV + reference texts) through
//! an ASR model and pipeline config, scores the output, and writes comparable
//! JSON + markdown reports. Drivable by hand or by an agent. See
//! `bench/README.md`.
//!
//! Entry point: [`run`], invoked by the `handy-bench` binary.

mod corpus;
mod engine;
mod history_export;
mod record;
mod report;
mod score;

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use crate::settings::{get_default_settings, AppSettings};

use self::corpus::Corpus;
use self::engine::{load_wav_16k, LoadedModel, ModelSpec};
use self::report::{CaseResult, ConfigOutcome, ConfigSpec, RunReport};

/// On/off flag used by `--text-rules` and `--itn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum OnOff {
    On,
    Off,
}

impl OnOff {
    fn enabled(self) -> bool {
        matches!(self, OnOff::On)
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "handy-bench",
    about = "Headless voice-dictation benchmark harness for the voice-control fork"
)]
struct BenchCli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Transcribe the corpus with each model×config, score, and write reports.
    Run(RunArgs),
    /// Turn real transcription history + recordings into corpus case stubs.
    ExportHistory(ExportArgs),
    /// Interactively record the corpus audio from the default microphone.
    Record(RecordArgs),
}

#[derive(Parser, Debug)]
struct RunArgs {
    /// Corpus directory (contains manifest.toml + <case>/<variant>.wav).
    #[arg(long, default_value = "bench/corpus")]
    corpus: PathBuf,
    /// Comma-separated model specs: `[name=][engine:]path`
    /// (engine = cpp|whisper|parakeet|canary).
    #[arg(long, value_delimiter = ',')]
    models: Vec<String>,
    /// Base dir for relative model paths (default: the HF hub cache).
    #[arg(long)]
    models_dir: Option<PathBuf>,
    /// Language code for the transcription, or `auto`.
    #[arg(long, default_value = "de")]
    language: String,
    /// Apply the deterministic text-rules layer (adds a `rules` config column).
    #[arg(long, default_value = "on")]
    text_rules: OnOff,
    /// Apply inverse text normalization within the `rules` config.
    #[arg(long, default_value = "on")]
    itn: OnOff,
    /// Settings source: `default` (real settings_store.json), `none` (built-in
    /// defaults), or a path to a settings JSON file.
    #[arg(long, default_value = "default")]
    settings: String,
    /// Output directory for the timestamped JSON + markdown reports.
    #[arg(long, default_value = "bench/results")]
    out: PathBuf,
}

#[derive(Parser, Debug)]
struct ExportArgs {
    /// App data dir holding history.db + recordings/ (default: discovered).
    #[arg(long)]
    app_data: Option<PathBuf>,
    /// Output corpus directory to write the WAVs + stub manifest into.
    #[arg(long, default_value = "bench/corpus")]
    out: PathBuf,
    /// Only export the most recent N entries.
    #[arg(long)]
    limit: Option<usize>,
}

#[derive(Parser, Debug)]
struct RecordArgs {
    /// Corpus directory (its manifest drives which sentences to record).
    #[arg(long, default_value = "bench/corpus")]
    corpus: PathBuf,
    /// Only record this case id.
    #[arg(long)]
    only: Option<String>,
    /// Re-record this case id, or one case/variant take. Repeatable.
    ///
    /// With --only, the effective set is the intersection. Without --only, only
    /// the requested takes are recorded.
    #[arg(long = "re-record")]
    re_record: Vec<String>,
    /// Re-record even if the WAV already exists.
    #[arg(long)]
    overwrite: bool,
}

/// CLI entry point for the `handy-bench` binary.
pub fn run() -> Result<()> {
    let cli = BenchCli::parse();
    match cli.command {
        Command::Run(args) => run_bench(args),
        Command::ExportHistory(args) => {
            history_export::export(&resolve_app_data(args.app_data)?, &args.out, args.limit)
        }
        Command::Record(args) => record::record(
            &args.corpus,
            args.only.as_deref(),
            args.overwrite,
            &args.re_record,
        ),
    }
}

fn run_bench(args: RunArgs) -> Result<()> {
    if args.models.is_empty() {
        bail!("--models is required (comma-separated engine-tagged paths)");
    }

    let models_dir = args.models_dir.clone().unwrap_or_else(default_hf_hub_dir);
    let specs: Vec<ModelSpec> = args
        .models
        .iter()
        .map(|m| ModelSpec::parse(m, &models_dir))
        .collect::<Result<_>>()?;

    let corpus = Corpus::load(&args.corpus)?;
    let base_settings = load_settings(&args.settings)?;
    let language = if args.language.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(args.language.clone())
    };

    // Configs form the matrix columns. `raw` is always present (text rules off);
    // `rules` is added when --text-rules on so the report shows their effect.
    let mut configs = vec![ConfigSpec {
        name: "raw".to_string(),
        rules: false,
        itn: false,
    }];
    if args.text_rules.enabled() {
        configs.push(ConfigSpec {
            name: "rules".to_string(),
            rules: true,
            itn: args.itn.enabled(),
        });
    }

    let mut results: Vec<CaseResult> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for spec in &specs {
        println!("Loading model `{}` ({:?})...", spec.name, spec.kind);
        let mut model = LoadedModel::load(spec)
            .with_context(|| format!("failed to load model `{}`", spec.name))?;

        for case in &corpus.manifest.cases {
            for variant in &case.variants {
                let audio_path = corpus.audio_path(&case.id, variant);
                if !audio_path.exists() {
                    skipped.push(format!("{}/{}", case.id, variant));
                    continue;
                }

                let audio = load_wav_16k(&audio_path)?;
                let raw = model.transcribe(&audio, language.as_deref())?;
                let recognition = score::recognition(&case.spoken, &raw);
                println!(
                    "  {} / {} / {}: WER {:.3}",
                    spec.name, case.id, variant, recognition.wer
                );

                let mut outcomes = Vec::with_capacity(configs.len());
                for cfg in &configs {
                    let mut s = base_settings.clone();
                    s.text_rules_enabled = cfg.rules;
                    s.text_rules_itn_enabled = cfg.itn;
                    // Mirror the app: custom words are only pre-prompted for
                    // whisper; here we always let the post-pipeline apply them.
                    let processed = crate::managers::transcription::post_process_transcription_text(
                        raw.clone(),
                        &s,
                        false,
                    );
                    let format = score::format(&case.expected, &processed);
                    outcomes.push(ConfigOutcome {
                        config: cfg.name.clone(),
                        processed,
                        format,
                    });
                }

                results.push(CaseResult {
                    case_id: case.id.clone(),
                    variant: variant.clone(),
                    tags: case.tags.clone(),
                    model: spec.name.clone(),
                    spoken: case.spoken.clone(),
                    expected: case.expected.clone(),
                    raw,
                    recognition,
                    configs: outcomes,
                });
            }
        }
    }

    if !skipped.is_empty() {
        eprintln!(
            "Skipped {} case/variant(s) with no recording (run `handy-bench record`): {}",
            skipped.len(),
            skipped.join(", ")
        );
    }
    if results.is_empty() {
        bail!(
            "no audio found under {} — record the corpus first with `handy-bench record`",
            corpus.root.display()
        );
    }

    let report = RunReport {
        timestamp: chrono::Local::now().format("%Y%m%d-%H%M%S").to_string(),
        corpus_root: corpus.root.display().to_string(),
        models: specs.iter().map(|s| s.name.clone()).collect(),
        configs,
        language: args.language.clone(),
        settings_source: args.settings.clone(),
        results,
    };

    let (json_path, md_path) = report.write(&args.out)?;
    println!(
        "\nWrote:\n  {}\n  {}",
        json_path.display(),
        md_path.display()
    );
    println!("\n{}", report.markdown());
    Ok(())
}

/// Load benchmark settings from the requested source.
fn load_settings(source: &str) -> Result<AppSettings> {
    match source {
        "none" => Ok(get_default_settings()),
        "default" => {
            let path = default_app_data_dir().join(crate::settings::SETTINGS_STORE_PATH);
            load_settings_file(&path).or_else(|e| {
                eprintln!(
                    "Could not load {} ({e}); falling back to built-in defaults.",
                    path.display()
                );
                Ok(get_default_settings())
            })
        }
        path => load_settings_file(Path::new(path)),
    }
}

/// Parse an `AppSettings` out of a `settings_store.json`-shaped file (the real
/// settings live under a top-level `"settings"` key).
fn load_settings_file(path: &Path) -> Result<AppSettings> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read settings file {}", path.display()))?;
    let value: serde_json::Value =
        serde_json::from_str(&text).context("settings file is not valid JSON")?;
    let inner = value.get("settings").cloned().unwrap_or(value);
    serde_json::from_value(inner).context("failed to deserialize AppSettings")
}

fn resolve_app_data(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let dir = explicit.unwrap_or_else(default_app_data_dir);
    if !dir.exists() {
        bail!(
            "app data dir not found: {} (pass --app-data)",
            dir.display()
        );
    }
    Ok(dir)
}

/// Best-effort home directory without pulling in the `dirs` crate.
fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Platform user cache dir (matches what `hf-hub`/`dirs` use).
fn cache_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home_dir().join("Library/Caches")
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData/Local"))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".cache"))
    }
}

/// Default `--models-dir`: the HuggingFace hub cache, where the running app
/// keeps its GGUF/ONNX models (see `engine.rs` module docs).
fn default_hf_hub_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("HF_HUB_CACHE") {
        return PathBuf::from(dir);
    }
    if let Some(home) = std::env::var_os("HF_HOME") {
        return PathBuf::from(home).join("hub");
    }
    cache_dir().join("huggingface").join("hub")
}

/// Default Handy app data dir (portable-aware paths are irrelevant headlessly).
fn default_app_data_dir() -> PathBuf {
    const BUNDLE_ID: &str = "com.pais.handy";
    #[cfg(target_os = "macos")]
    {
        home_dir()
            .join("Library/Application Support")
            .join(BUNDLE_ID)
    }
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join("AppData/Roaming"))
            .join(BUNDLE_ID)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home_dir().join(".local/share"))
            .join(BUNDLE_ID)
    }
}
