//! Long-audio recording-limit probe for the benchmark harness (fork feature:
//! voice-control).
//!
//! Some speech models truncate their output beyond a certain audio duration
//! (e.g. Qwen3-ASR hits transcribe-cpp's output-token cap around ~45 s). The app
//! auto-stops recordings via the per-model table in
//! `managers::model::recording_limit_for_model_id`, but those ceilings are
//! hand-guessed. `probe-limit` measures the real ceiling per model so a
//! new-model evaluation can fill that table programmatically.
//!
//! Strategy: build ever-longer test audio by cycling 2–3 corpus samples in
//! round-robin (with short silence gaps), transcribe each length, and call a
//! length *healthy* when the transcribed word count stays within
//! `--health-threshold` of the reference word count. A coarse doubling search
//! finds the first unhealthy length, then a linear refinement at `--step-secs`
//! granularity pins the boundary. The recommendation subtracts a safety margin
//! from the last healthy length.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use serde::Serialize;

use super::corpus::{Case, Corpus};
use super::engine::{load_wav_16k, LoadedModel, ModelSpec};

/// 16 kHz mono, matching `load_wav_16k`.
const SAMPLE_RATE: usize = 16_000;
/// Silence inserted between concatenated repeats.
const GAP_SECS: f64 = 0.4;
/// Warning lead time written into the ready-to-paste snippet. Mirrors the
/// existing `recording_limit_for_model_id` entry; a human tunes it if needed.
const SNIPPET_WARNING_MS: u64 = 10_000;

/// Arguments for the `probe-limit` subcommand.
#[derive(clap::Parser, Debug)]
pub struct ProbeArgs {
    /// Comma-separated model specs: `[name=][engine:]path` (same grammar as
    /// `run`).
    #[arg(long, value_delimiter = ',')]
    pub models: Vec<String>,
    /// Base dir for relative model paths (default: the HF hub cache).
    #[arg(long)]
    pub models_dir: Option<PathBuf>,
    /// Corpus directory (contains manifest.toml + <case>/<variant>.wav).
    #[arg(long, default_value = "bench/corpus")]
    pub corpus: PathBuf,
    /// Explicit case ids to cycle (comma-separated). Default: auto-pick 2–3.
    #[arg(long, value_delimiter = ',')]
    pub cases: Vec<String>,
    /// Audio variant to use for every case.
    #[arg(long, default_value = "normal")]
    pub variant: String,
    /// Granularity of the answer, in seconds.
    #[arg(long, default_value_t = 5)]
    pub step_secs: u32,
    /// Give up above this duration (report "no limit found ≤ max").
    #[arg(long, default_value_t = 240)]
    pub max_secs: u32,
    /// Safety margin subtracted from the last healthy duration.
    #[arg(long, default_value_t = 10)]
    pub margin_secs: u32,
    /// Minimum transcribed/reference word-count ratio to count as healthy.
    #[arg(long, default_value_t = 0.7)]
    pub health_threshold: f64,
    /// Language code for the transcription, or `auto`.
    #[arg(long, default_value = "auto")]
    pub language: String,
    /// Output directory for the timestamped JSON + markdown reports.
    #[arg(long, default_value = "bench/results")]
    pub out: PathBuf,
}

/// A selected corpus sample: its id, decoded 16 kHz audio and reference word
/// count (whitespace-split `spoken`).
struct Sample {
    id: String,
    audio: Vec<f32>,
    words: usize,
}

/// One probed duration and its verdict (serialized into the report).
#[derive(Debug, Clone, Serialize)]
pub struct ProbePoint {
    /// Target duration the search asked for.
    pub target_secs: u32,
    /// Actual concatenated audio duration (whole samples slightly overshoot).
    pub actual_secs: f64,
    /// Reference word count of every included repeat.
    pub ref_words: usize,
    /// Words in the transcription (0 on a truncation error).
    pub got_words: usize,
    /// `got_words / ref_words`.
    pub ratio: f64,
    /// `healthy`, `unhealthy`, or `truncated` (engine reported a cut-off).
    pub verdict: String,
}

/// The probe result for one model.
#[derive(Debug, Clone, Serialize)]
pub struct ModelProbe {
    pub model: String,
    /// Largest healthy target duration, if any.
    pub last_healthy_secs: Option<u32>,
    /// Smallest unhealthy target duration, if the ceiling was found ≤ max.
    pub first_unhealthy_secs: Option<u32>,
    /// `(last_healthy - margin).max(step) * 1000`, only when a ceiling was found.
    pub recommended_max_recording_ms: Option<u64>,
    /// Human summary of the recommendation.
    pub recommendation: String,
    /// Ready-to-paste `recording_limit_for_model_id` match arm (when limited).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
    /// Every probed point, in probe order.
    pub points: Vec<ProbePoint>,
    /// Set when the model's probe aborted on an unexpected transcription error.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A complete `probe-limit` run.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeReport {
    pub timestamp: String,
    pub corpus_root: String,
    pub language: String,
    pub variant: String,
    pub step_secs: u32,
    pub max_secs: u32,
    pub margin_secs: u32,
    pub health_threshold: f64,
    pub selected_cases: Vec<String>,
    pub models: Vec<ModelProbe>,
}

/// CLI entry point for `handy-bench probe-limit`.
pub fn probe_limit(args: ProbeArgs) -> Result<()> {
    if args.models.is_empty() {
        bail!("--models is required (comma-separated engine-tagged paths)");
    }
    if args.step_secs == 0 {
        bail!("--step-secs must be > 0");
    }
    if args.max_secs < args.step_secs {
        bail!(
            "--max-secs ({}) must be >= --step-secs ({})",
            args.max_secs,
            args.step_secs
        );
    }
    if !(0.0..=1.0).contains(&args.health_threshold) {
        bail!("--health-threshold must be in 0.0..=1.0");
    }

    let models_dir = args
        .models_dir
        .clone()
        .unwrap_or_else(super::default_hf_hub_dir);
    let specs: Vec<ModelSpec> = args
        .models
        .iter()
        .map(|m| ModelSpec::parse(m, &models_dir))
        .collect::<Result<_>>()?;

    let corpus = Corpus::load(&args.corpus)?;
    let language = if args.language.eq_ignore_ascii_case("auto") {
        None
    } else {
        Some(args.language.clone())
    };

    // Resolve which cases to cycle, then decode their audio once.
    let cases = select_cases(&corpus, &args.variant, &args.cases)?;
    let samples: Vec<Sample> = cases
        .iter()
        .map(|case| {
            let path = corpus.audio_path(&case.id, &args.variant);
            let audio = load_wav_16k(&path)
                .with_context(|| format!("failed to load {}", path.display()))?;
            Ok(Sample {
                id: case.id.clone(),
                words: word_count(&case.spoken),
                audio,
            })
        })
        .collect::<Result<_>>()?;

    println!(
        "Probing recording limit over cases [{}] (variant `{}`), step {}s, max {}s, threshold {:.2}.\n",
        samples.iter().map(|s| s.id.as_str()).collect::<Vec<_>>().join(", "),
        args.variant,
        args.step_secs,
        args.max_secs,
        args.health_threshold,
    );

    let mut model_probes = Vec::with_capacity(specs.len());
    for spec in &specs {
        println!("Loading model `{}` ({:?})...", spec.name, spec.kind);
        let mut model = LoadedModel::load(spec)
            .with_context(|| format!("failed to load model `{}`", spec.name))?;
        model_probes.push(probe_one_model(
            &spec.name,
            &mut model,
            &samples,
            language.as_deref(),
            &args,
        ));
        println!();
    }

    let report = ProbeReport {
        timestamp: chrono::Local::now().format("%Y%m%d-%H%M%S").to_string(),
        corpus_root: corpus.root.display().to_string(),
        language: args.language.clone(),
        variant: args.variant.clone(),
        step_secs: args.step_secs,
        max_secs: args.max_secs,
        margin_secs: args.margin_secs,
        health_threshold: args.health_threshold,
        selected_cases: samples.iter().map(|s| s.id.clone()).collect(),
        models: model_probes,
    };

    let (json_path, md_path) = report.write(&args.out)?;
    println!("Wrote:\n  {}\n  {}", json_path.display(), md_path.display());
    println!("\n{}", report.markdown());
    Ok(())
}

/// Drive the adaptive search for a single loaded model, transcribing each
/// probed length and printing the verdict.
fn probe_one_model(
    name: &str,
    model: &mut LoadedModel,
    samples: &[Sample],
    language: Option<&str>,
    args: &ProbeArgs,
) -> ModelProbe {
    let gap_samples = (GAP_SECS * SAMPLE_RATE as f64).round() as usize;
    let mut points: Vec<ProbePoint> = Vec::new();
    let mut abort: Option<String> = None;

    let search = drive_search(args.step_secs, args.max_secs, |target| {
        // Build the concatenated audio for this target length.
        let plan = round_robin_plan(
            &samples.iter().map(sample_secs).collect::<Vec<_>>(),
            GAP_SECS,
            target as f64,
        );
        let mut audio: Vec<f32> = Vec::new();
        let mut ref_words = 0usize;
        for (i, &idx) in plan.iter().enumerate() {
            if i > 0 {
                audio.resize(audio.len() + gap_samples, 0.0);
            }
            audio.extend_from_slice(&samples[idx].audio);
            ref_words += samples[idx].words;
        }
        let actual_secs = audio.len() as f64 / SAMPLE_RATE as f64;

        // Transcribe; a truncation error is an unhealthy verdict, any other
        // error aborts this model's probe.
        let (got_words, verdict, healthy) = match model.transcribe(&audio, language) {
            Ok(text) => {
                let got = word_count(&text);
                let healthy = is_healthy(got, ref_words, args.health_threshold);
                let verdict = if healthy { "healthy" } else { "unhealthy" };
                (got, verdict, healthy)
            }
            Err(err) if error_indicates_truncation(&err.to_string()) => (0, "truncated", false),
            Err(err) => {
                bail!("transcription failed at {target}s: {err:#}");
            }
        };

        let ratio = ratio(got_words, ref_words);
        println!(
            "  {name}: {actual_secs:6.1}s  ref {ref_words:4}  got {got_words:4}  ratio {ratio:5.2}  -> {verdict}"
        );
        points.push(ProbePoint {
            target_secs: target,
            actual_secs,
            ref_words,
            got_words,
            ratio,
            verdict: verdict.to_string(),
        });
        Ok(healthy)
    });

    let outcome = match search {
        Ok(outcome) => outcome,
        Err(err) => {
            abort = Some(format!("{err:#}"));
            SearchOutcome::default()
        }
    };

    build_model_probe(name, points, outcome, abort, args)
}

/// Assemble the per-model result (boundaries, recommendation, snippet).
fn build_model_probe(
    name: &str,
    points: Vec<ProbePoint>,
    outcome: SearchOutcome,
    abort: Option<String>,
    args: &ProbeArgs,
) -> ModelProbe {
    let mut recommended = None;
    let mut snippet = None;
    let recommendation = if abort.is_some() {
        "probe aborted before a verdict".to_string()
    } else if let Some(unhealthy) = outcome.first_unhealthy {
        let last = outcome.last_healthy.unwrap_or(0);
        let ms = recommended_max_ms(last, args.margin_secs, args.step_secs);
        recommended = Some(ms);
        snippet = Some(limit_snippet(name, ms));
        if outcome.last_healthy.is_some() {
            format!(
                "truncates at {unhealthy}s (last healthy {last}s) -> recommend max_recording_ms = {ms}"
            )
        } else {
            format!(
                "truncates even at the shortest probe ({unhealthy}s) -> recommend floor max_recording_ms = {ms}"
            )
        }
    } else {
        format!("no limit needed up to {}s", args.max_secs)
    };

    ModelProbe {
        model: name.to_string(),
        last_healthy_secs: outcome.last_healthy,
        first_unhealthy_secs: outcome.first_unhealthy,
        recommended_max_recording_ms: recommended,
        recommendation,
        snippet,
        points,
        error: abort,
    }
}

/// Reference length of a sample in seconds.
fn sample_secs(s: &Sample) -> f64 {
    s.audio.len() as f64 / SAMPLE_RATE as f64
}

/// Pick the cases to cycle. Explicit `--cases` wins; otherwise prefer `prose`
/// cases with the variant's wav present, then fill with the longest `spoken`
/// texts, up to 3 distinct cases. Errors if fewer than 2 usable wavs exist.
fn select_cases<'a>(
    corpus: &'a Corpus,
    variant: &str,
    explicit: &[String],
) -> Result<Vec<&'a Case>> {
    let has_wav = |case: &Case| corpus.audio_path(&case.id, variant).exists();

    let selected: Vec<&Case> = if explicit.is_empty() {
        // Auto-select: prose first, then the longest `spoken` texts.
        let usable: Vec<&Case> = corpus
            .manifest
            .cases
            .iter()
            .filter(|c| has_wav(c))
            .collect();
        let mut prose: Vec<&Case> = usable
            .iter()
            .copied()
            .filter(|c| c.tags.iter().any(|t| t == "prose"))
            .collect();
        let mut rest: Vec<&Case> = usable
            .iter()
            .copied()
            .filter(|c| !c.tags.iter().any(|t| t == "prose"))
            .collect();
        rest.sort_by_key(|c| std::cmp::Reverse(word_count(&c.spoken)));
        prose.append(&mut rest);
        prose.into_iter().take(3).collect()
    } else {
        // Explicit ids, in the given order.
        explicit
            .iter()
            .map(|id| {
                let case = corpus
                    .manifest
                    .cases
                    .iter()
                    .find(|c| &c.id == id)
                    .with_context(|| format!("case `{id}` not found in corpus manifest"))?;
                if !has_wav(case) {
                    bail!(
                        "case `{id}` has no `{variant}` recording at {}",
                        corpus.audio_path(id, variant).display()
                    );
                }
                Ok(case)
            })
            .collect::<Result<_>>()?
    };

    if selected.len() < 2 {
        bail!(
            "need at least 2 cases with a `{variant}` recording under {} (found {}); \
             record more of the corpus or pass --cases",
            corpus.root.display(),
            selected.len()
        );
    }
    Ok(selected)
}

/// Whitespace-split word count.
fn word_count(text: &str) -> usize {
    text.split_whitespace().count()
}

/// Transcribed/reference word ratio (0.0 when there is no reference).
fn ratio(got: usize, reference: usize) -> f64 {
    if reference == 0 {
        0.0
    } else {
        got as f64 / reference as f64
    }
}

/// A length is healthy when its word ratio meets the threshold.
fn is_healthy(got: usize, reference: usize, threshold: f64) -> bool {
    ratio(got, reference) >= threshold
}

/// Heuristic: does a transcription error message point at output truncation
/// (as opposed to a load/backend failure)?
fn error_indicates_truncation(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "truncat",
        "max tokens",
        "max_tokens",
        "token limit",
        "output token",
        "n_max",
        "too long",
        "context window",
        "exceed",
    ]
    .iter()
    .any(|needle| m.contains(needle))
}

/// `(last_healthy - margin).max(step) * 1000`.
fn recommended_max_ms(last_healthy_secs: u32, margin_secs: u32, step_secs: u32) -> u64 {
    let secs = last_healthy_secs.saturating_sub(margin_secs).max(step_secs);
    secs as u64 * 1000
}

/// Render a ready-to-paste `recording_limit_for_model_id` match arm.
fn limit_snippet(model_name: &str, max_recording_ms: u64) -> String {
    let key = model_name.to_ascii_lowercase();
    format!(
        "if model_id.contains(\"{key}\") {{\n    \
         return Some(RecordingLimit {{\n        \
         max_recording_ms: {max_recording_ms},\n        \
         warning_ms: {SNIPPET_WARNING_MS},\n    }});\n}}"
    )
}

/// Coarse doubling grid: `step, 2·step, 4·step, …`, always ending at `max` so
/// the ceiling itself is probed. Assumes `0 < step <= max`.
fn coarse_grid(step: u32, max: u32) -> Vec<u32> {
    let mut grid = Vec::new();
    let mut d = step;
    while d < max {
        grid.push(d);
        d = d.saturating_mul(2);
    }
    grid.push(max);
    grid
}

/// Linear refinement grid between `lo` (last healthy) and `hi` (first unhealthy)
/// at `step` granularity: `lo+step, lo+2·step, … < hi`.
fn refine_grid(lo: u32, hi: u32, step: u32) -> Vec<u32> {
    let mut grid = Vec::new();
    let mut d = lo + step;
    while d < hi {
        grid.push(d);
        d += step;
    }
    grid
}

/// Boundaries found by the adaptive search (target seconds).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SearchOutcome {
    last_healthy: Option<u32>,
    first_unhealthy: Option<u32>,
}

/// Adaptive search: coarse doubling until the first unhealthy length (or max),
/// then linear refinement between the last healthy and first unhealthy length.
/// `is_healthy(target)` transcribes and returns the verdict; its `Err` aborts.
fn drive_search(
    step: u32,
    max: u32,
    mut is_healthy: impl FnMut(u32) -> Result<bool>,
) -> Result<SearchOutcome> {
    let mut out = SearchOutcome::default();

    for d in coarse_grid(step, max) {
        if is_healthy(d)? {
            out.last_healthy = Some(d);
        } else {
            out.first_unhealthy = Some(d);
            break;
        }
    }

    if let (Some(lo), Some(hi)) = (out.last_healthy, out.first_unhealthy) {
        for d in refine_grid(lo, hi, step) {
            if is_healthy(d)? {
                out.last_healthy = Some(d);
            } else {
                out.first_unhealthy = Some(d);
                break;
            }
        }
    }

    Ok(out)
}

/// Cycle sample indices `0,1,…,n-1,0,1,…` (with `gap` between repeats) until the
/// cumulative duration first reaches `target`; return the indices to append in
/// order. Only whole samples, so the actual duration may slightly exceed
/// `target`. Always includes at least one sample.
fn round_robin_plan(sample_secs: &[f64], gap: f64, target: f64) -> Vec<usize> {
    let n = sample_secs.len();
    let mut indices = Vec::new();
    let mut total = 0.0;
    // Guard against zero-length samples looping forever.
    let cap = 1_000_000;
    let mut i = 0usize;
    while total < target && i < cap {
        let idx = i % n;
        if i > 0 {
            total += gap;
        }
        total += sample_secs[idx];
        indices.push(idx);
        i += 1;
    }
    indices
}

impl ProbeReport {
    /// Write `probe-<timestamp>.json` and `probe-<timestamp>.md` into `out_dir`.
    /// Returns `(json_path, md_path)`.
    pub fn write(&self, out_dir: &std::path::Path) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("failed to create output dir {}", out_dir.display()))?;

        let json_path = out_dir.join(format!("probe-{}.json", self.timestamp));
        let md_path = out_dir.join(format!("probe-{}.md", self.timestamp));

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&json_path, json)
            .with_context(|| format!("failed to write {}", json_path.display()))?;
        std::fs::write(&md_path, self.markdown())
            .with_context(|| format!("failed to write {}", md_path.display()))?;

        Ok((json_path, md_path))
    }

    /// Render the human-readable markdown report.
    pub fn markdown(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "# Handy recording-limit probe\n");
        let _ = writeln!(s, "- Run: `{}`", self.timestamp);
        let _ = writeln!(s, "- Corpus: `{}`", self.corpus_root);
        let _ = writeln!(s, "- Language: `{}`", self.language);
        let _ = writeln!(s, "- Variant: `{}`", self.variant);
        let _ = writeln!(
            s,
            "- Step / max / margin: `{}s` / `{}s` / `{}s`",
            self.step_secs, self.max_secs, self.margin_secs
        );
        let _ = writeln!(s, "- Health threshold: `{:.2}`", self.health_threshold);
        let _ = writeln!(s, "- Cases: {}\n", self.selected_cases.join(", "));

        for m in &self.models {
            let _ = writeln!(s, "## {}\n", m.model);
            if let Some(err) = &m.error {
                let _ = writeln!(s, "**Aborted:** {err}\n");
            }
            let _ = writeln!(s, "- Recommendation: {}", m.recommendation);
            if let Some(ms) = m.recommended_max_recording_ms {
                let _ = writeln!(s, "- `recommended_max_recording_ms`: {ms}");
            }
            let _ = writeln!(s);

            let _ = writeln!(
                s,
                "| Target | Actual | Ref words | Got words | Ratio | Verdict |"
            );
            let _ = writeln!(s, "| --- | --- | --- | --- | --- | --- |");
            for p in &m.points {
                let _ = writeln!(
                    s,
                    "| {}s | {:.1}s | {} | {} | {:.2} | {} |",
                    p.target_secs, p.actual_secs, p.ref_words, p.got_words, p.ratio, p.verdict
                );
            }
            let _ = writeln!(s);

            if let Some(snippet) = &m.snippet {
                let _ = writeln!(
                    s,
                    "Paste into `recording_limit_for_model_id` (adjust the id substring):\n"
                );
                let _ = writeln!(s, "```rust\n{snippet}\n```\n");
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coarse_grid_doubles_and_ends_at_max() {
        assert_eq!(coarse_grid(5, 240), vec![5, 10, 20, 40, 80, 160, 240]);
        assert_eq!(coarse_grid(5, 20), vec![5, 10, 20]);
        // Exact doubling landing on max produces no duplicate.
        assert_eq!(coarse_grid(5, 80), vec![5, 10, 20, 40, 80]);
        // step == max yields a single point.
        assert_eq!(coarse_grid(5, 5), vec![5]);
    }

    #[test]
    fn refine_grid_is_open_interval() {
        assert_eq!(
            refine_grid(80, 160, 5),
            vec![85, 90, 95, 100, 105, 110, 115, 120, 125, 130, 135, 140, 145, 150, 155]
        );
        // Adjacent boundaries leave nothing to refine.
        assert_eq!(refine_grid(10, 15, 5), Vec::<u32>::new());
        assert_eq!(refine_grid(10, 20, 5), vec![15]);
    }

    /// The search should probe the doubling grid until the first unhealthy
    /// point, then refine linearly, and land on the true ceiling.
    #[test]
    fn drive_search_doubles_then_refines() {
        // Model stays healthy up to and including 47 s.
        let ceiling = 47u32;
        let mut probed = Vec::new();
        let outcome = drive_search(5, 240, |d| {
            probed.push(d);
            Ok(d <= ceiling)
        })
        .unwrap();

        // Coarse: 5,10,20,40 healthy; 80 unhealthy -> stop. Refine 45,50: 45
        // healthy, 50 unhealthy -> stop.
        assert_eq!(probed, vec![5, 10, 20, 40, 80, 45, 50]);
        assert_eq!(outcome.last_healthy, Some(45));
        assert_eq!(outcome.first_unhealthy, Some(50));
    }

    #[test]
    fn drive_search_no_limit_when_all_healthy() {
        let outcome = drive_search(5, 20, |_| Ok(true)).unwrap();
        assert_eq!(outcome.last_healthy, Some(20));
        assert_eq!(outcome.first_unhealthy, None);
    }

    #[test]
    fn drive_search_unhealthy_from_the_start() {
        let mut probed = Vec::new();
        let outcome = drive_search(5, 40, |d| {
            probed.push(d);
            Ok(false)
        })
        .unwrap();
        // First coarse point already unhealthy: no healthy, no refinement.
        assert_eq!(probed, vec![5]);
        assert_eq!(outcome.last_healthy, None);
        assert_eq!(outcome.first_unhealthy, Some(5));
    }

    #[test]
    fn drive_search_propagates_errors() {
        let err = drive_search(5, 40, |_| bail!("boom")).unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[test]
    fn health_decision_uses_threshold() {
        assert!(is_healthy(7, 10, 0.7));
        assert!(is_healthy(10, 10, 0.7));
        assert!(!is_healthy(6, 10, 0.7));
        // No reference words is never healthy.
        assert!(!is_healthy(5, 0, 0.7));
    }

    #[test]
    fn recommendation_math() {
        assert_eq!(recommended_max_ms(40, 10, 5), 30_000);
        assert_eq!(recommended_max_ms(40, 5, 5), 35_000);
        // Margin exceeding the healthy length floors to one step.
        assert_eq!(recommended_max_ms(8, 10, 5), 5_000);
        assert_eq!(recommended_max_ms(5, 10, 5), 5_000);
    }

    #[test]
    fn truncation_error_detection() {
        assert!(error_indicates_truncation(
            "transcribe-cpp transcription failed: output truncated at max tokens"
        ));
        assert!(error_indicates_truncation("context window exceeded"));
        assert!(!error_indicates_truncation(
            "failed to create transcribe-cpp session: no such file"
        ));
    }

    /// Total duration of a plan: sample lengths plus a gap between each repeat.
    fn plan_secs(sample_secs: &[f64], gap: f64, indices: &[usize]) -> f64 {
        let gaps = indices.len().saturating_sub(1) as f64 * gap;
        indices.iter().map(|&i| sample_secs[i]).sum::<f64>() + gaps
    }

    #[test]
    fn round_robin_cycles_until_target() {
        // Three 10 s samples, 0.4 s gaps, target 25 s.
        let lens = [10.0, 10.0, 10.0];
        let plan = round_robin_plan(&lens, 0.4, 25.0);
        assert_eq!(plan, vec![0, 1, 2]);
        assert!((plan_secs(&lens, 0.4, &plan) - 30.8).abs() < 1e-9);

        // Two samples of different length cycle round-robin past the target.
        let lens = [10.0, 8.0];
        let plan = round_robin_plan(&lens, 0.4, 30.0);
        assert_eq!(plan, vec![0, 1, 0, 1]);
        assert!((plan_secs(&lens, 0.4, &plan) - 37.2).abs() < 1e-9);

        // Always at least one sample, even below the first length.
        let plan = round_robin_plan(&[10.0, 10.0], 0.4, 3.0);
        assert_eq!(plan, vec![0]);
    }
}
