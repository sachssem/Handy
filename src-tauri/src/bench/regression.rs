//! Real-dictation regression gate. The provider/app-context stages are diagnostic
//! only; the deterministic output uses the very same function as live dictation.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::Parser;
use hf_hub::{Cache, Repo, RepoType};
use serde::Serialize;

use super::corpus::{Case, CaseStatus, Corpus};
use super::engine::{load_wav_16k, LoadedModel, ModelSpec};
use super::score;
use crate::audio_toolkit::text::OutputLanguageEvidence;
use crate::settings::{get_default_settings, AppSettings};

#[derive(Debug, Parser)]
pub(super) struct RegressionArgs {
    #[arg(long, default_value = "bench/corpus/regression")]
    corpus: PathBuf,
    /// Engine-tagged path; otherwise resolve the selected model from the HF cache.
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    models_dir: Option<PathBuf>,
    /// `default`, `none`, or a settings_store.json-shaped file (including learned state).
    #[arg(long, default_value = "default")]
    settings: String,
    /// `auto` uses each case's de/en tag for deterministic language-gated stages.
    #[arg(long, default_value = "auto")]
    language: String,
    #[arg(long, default_value = "bench/results")]
    out: PathBuf,
    /// Print/report failures without returning a failing exit status.
    #[arg(long)]
    no_fail: bool,
}

#[derive(Debug, Serialize)]
struct Row {
    case_id: String,
    variant: String,
    status: String,
    exact: bool,
    normalized: bool,
    spoken_wer: Option<f64>,
    target: String,
    raw: String,
    output: String,
    error: Option<String>,
}

#[derive(Serialize)]
struct Report {
    timestamp: String,
    model: String,
    model_path: String,
    corpus: String,
    settings: String,
    learned_corrections_enabled: bool,
    active_learned_corrections: usize,
    excluded_stages: [&'static str; 3],
    pass: usize,
    soft_pass: usize,
    fail: usize,
    llm_dependent: usize,
    pending_truth: usize,
    rows: Vec<Row>,
}

fn outcome(case: &Case, text: &str) -> (&'static str, bool, bool) {
    let exact = case.expected.trim() == text.trim();
    let normalized = score::normalize(&case.expected) == score::normalize(text);
    let status = if case.needs_llm {
        "LLM-dependent"
    } else if exact {
        "PASS"
    } else if case.case_insensitive_path && case.expected.trim().eq_ignore_ascii_case(text.trim()) {
        "SOFT-PASS"
    } else {
        "FAIL"
    };
    (status, exact, normalized)
}

/// Use hf-hub itself, including its HF_HOME handling and pinned
/// revision → main fallback, exactly as the app does. Never download a model.
fn selected_model(settings: &AppSettings) -> Result<ModelSpec> {
    use crate::managers::model::ModelSource;
    let selected = &settings.selected_model;
    for descriptor in crate::catalog::CATALOG.iter() {
        let ModelSource::HuggingFace { repo_id, revision } = &descriptor.source else {
            continue;
        };
        for file in &descriptor.files {
            if selected != &format!("{repo_id}/{}", file.filename) {
                continue;
            }
            let cache = Cache::from_env();
            let get = |rev: &str| {
                cache
                    .repo(Repo::with_revision(
                        repo_id.clone(),
                        RepoType::Model,
                        rev.into(),
                    ))
                    .get(&file.filename)
            };
            let path = get(revision).or_else(|| get("main")).with_context(|| {
                format!("selected model `{selected}` is not in the HF cache; pass --model cpp:/path/model.gguf")
            })?;
            return ModelSpec::parse(&format!("cpp:{}", path.display()), &PathBuf::new());
        }
    }
    bail!("selected model `{selected}` is not a cached catalog GGUF; pass --model [engine:]path")
}

/// Preload a memory-only Tauri store so `apply_learned` sees the real snapshot.
/// This builds no windows, starts no app/observer loop, and cannot save settings.
/// The store serializer rejects even explicit migration/exit writes.
struct PipelineRuntime(tauri::App);

impl Drop for PipelineRuntime {
    fn drop(&mut self) {
        self.0.cleanup_before_exit();
    }
}

fn pipeline_store(source: &str) -> Result<(PipelineRuntime, AppSettings)> {
    let (settings, mut values): (AppSettings, HashMap<String, serde_json::Value>) =
        if source == "none" {
            (get_default_settings(), HashMap::new())
        } else {
            let path = if source == "default" {
                super::default_app_data_dir().join(crate::settings::SETTINGS_STORE_PATH)
            } else {
                PathBuf::from(source)
            };
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("failed to read settings snapshot {}", path.display()))?;
            let value: serde_json::Value = serde_json::from_str(&text)?;
            let settings: AppSettings = serde_json::from_value(
                value
                    .get("settings")
                    .cloned()
                    .unwrap_or_else(|| value.clone()),
            )?;
            let values = if value.get("settings").is_some() {
                serde_json::from_value(value)?
            } else {
                HashMap::new()
            };
            (settings, values)
        };
    values.insert("settings".into(), serde_json::to_value(&settings)?);
    // A second generate_context! would embed the macOS Info.plist symbol twice.
    // This windowless context needs neither bundled frontend assets nor IPC ACLs.
    let context = tauri::Context::new(
        tauri::Config {
            identifier: "com.pais.handy".into(),
            ..Default::default()
        },
        Box::new(tauri::utils::assets::EmbeddedAssets::new(
            Default::default(),
            &[],
            Default::default(),
        )),
        None,
        None,
        tauri::utils::PackageInfo {
            name: "handy-bench".into(),
            version: env!("CARGO_PKG_VERSION").parse()?,
            authors: env!("CARGO_PKG_AUTHORS"),
            description: "Headless regression store",
            crate_name: env!("CARGO_PKG_NAME"),
        },
        tauri::Pattern::Brownfield,
        // Tauri's default dynamic-acl feature takes both maps in release too.
        tauri::ipc::RuntimeAuthority::new(Default::default(), Default::default()),
        None,
    );
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .build(context)
        .context("cannot initialize the windowless learned-correction store")?;
    let store = tauri_plugin_store::StoreBuilder::new(&app, crate::settings::SETTINGS_STORE_PATH)
        .defaults(values)
        .create_new()
        .disable_auto_save()
        .serialize(|_| Err(std::io::Error::other("benchmark store is read-only").into()))
        .build()?;
    crate::correction_learning::init(app.handle());
    // The resource table retains the store for settings gating until app drop.
    drop(store);
    Ok((PipelineRuntime(app), settings))
}

impl Report {
    fn table(&self) -> String {
        let mut table = String::from("| Case / take | Result | Exact | Norm | WER | Final text / error |\n| --- | --- | --- | --- | --- | --- |\n");
        for row in &self.rows {
            let text = row
                .error
                .as_ref()
                .unwrap_or(&row.output)
                .replace('\n', "\\n")
                .replace('|', "\\|");
            let wer = row
                .spoken_wer
                .map(|w| format!("{w:.3}"))
                .unwrap_or_else(|| "—".into());
            let _ = writeln!(
                table,
                "| {} / {} | {} | {} | {} | {} | {} |",
                row.case_id, row.variant, row.status, row.exact, row.normalized, wer, text
            );
        }
        let _ = writeln!(table, "\n{} pass ({} soft), {} fail; {} LLM-dependent, {} pending truth.\nNormalized match ignores punctuation/case and is diagnostic only; only the explicit path tolerance permits a soft pass.", self.pass, self.soft_pass, self.fail, self.llm_dependent, self.pending_truth);
        table
    }
}

pub(super) fn run(args: RegressionArgs) -> Result<()> {
    let corpus = Corpus::load(&args.corpus)?;
    let (_app, settings) = pipeline_store(&args.settings)?;
    let spec = match args.model.as_deref() {
        Some(raw) => ModelSpec::parse(
            raw,
            &args.models_dir.unwrap_or_else(super::default_hf_hub_dir),
        )?,
        None => selected_model(&settings)?,
    };
    println!("Loading {} from {}", spec.name, spec.path.display());
    let mut model = LoadedModel::load(&spec)?;
    let mut report = Report {
        timestamp: chrono::Local::now().format("%Y%m%d-%H%M%S-%3f").to_string(),
        model: spec.name,
        model_path: spec.path.display().to_string(),
        corpus: args.corpus.display().to_string(),
        settings: args.settings,
        learned_corrections_enabled: settings.learn_corrections_enabled,
        active_learned_corrections: crate::correction_learning::snapshot()
            .corrections
            .iter()
            .filter(|c| c.is_applied())
            .count(),
        excluded_stages: [
            "self-correction LLM",
            "app styles",
            "optional post-process provider",
        ],
        pass: 0,
        soft_pass: 0,
        fail: 0,
        llm_dependent: 0,
        pending_truth: 0,
        rows: Vec::new(),
    };
    for case in &corpus.manifest.cases {
        for variant in &case.variants {
            let mut row = Row {
                case_id: case.id.clone(),
                variant: variant.clone(),
                status: "pending_truth".into(),
                exact: false,
                normalized: false,
                spoken_wer: None,
                target: case.expected.clone(),
                raw: String::new(),
                output: String::new(),
                error: None,
            };
            if case.status == CaseStatus::PendingTruth
                || case.expected.trim().is_empty()
                || case.expected.trim() == "TODO"
            {
                report.pending_truth += 1;
                report.rows.push(row);
                continue;
            }
            let language = if args.language.eq_ignore_ascii_case("auto") {
                case.tags
                    .iter()
                    .find(|tag| matches!(tag.as_str(), "de" | "en"))
                    .map(String::as_str)
            } else {
                Some(args.language.as_str())
            };
            let evidence = language
                .map(|l| OutputLanguageEvidence::UserSelected(l.into()))
                .unwrap_or(OutputLanguageEvidence::Unknown);
            let result = load_wav_16k(&corpus.audio_path(&case.id, variant))
                .and_then(|audio| model.transcribe(&audio, language));
            match result {
                Ok(raw) => {
                    row.spoken_wer = Some(score::wer(&case.spoken, &raw));
                    row.output = crate::managers::transcription::post_process_transcription_text(
                        raw.clone(),
                        &settings,
                        false,
                        &evidence,
                        &[],
                    );
                    row.raw = raw;
                    let (status, exact, normalized) = outcome(case, &row.output);
                    row.status = status.into();
                    row.exact = exact;
                    row.normalized = normalized;
                }
                Err(e) => {
                    row.status = if case.needs_llm {
                        "LLM-dependent"
                    } else {
                        "FAIL"
                    }
                    .into();
                    row.error = Some(format!("{e:#}"));
                }
            }
            match row.status.as_str() {
                "PASS" => report.pass += 1,
                "SOFT-PASS" => {
                    report.pass += 1;
                    report.soft_pass += 1;
                }
                "LLM-dependent" => report.llm_dependent += 1,
                _ => report.fail += 1,
            }
            println!("  {}: {}", row.case_id, row.status);
            report.rows.push(row);
        }
    }
    std::fs::create_dir_all(&args.out)?;
    let stem = args.out.join(format!("regression-{}", report.timestamp));
    std::fs::write(
        stem.with_extension("json"),
        serde_json::to_string_pretty(&report)?,
    )?;
    let table = report.table();
    std::fs::write(stem.with_extension("md"), &table)?;
    println!("\n{table}\nReports: {}.{{json,md}}", stem.display());
    if report.pass + report.fail == 0 {
        bail!("no deterministic cases were scored");
    }
    if report.fail > 0 && !args.no_fail {
        bail!("{} regression failure(s)", report.fail);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalization_never_hides_punctuation_failure() {
        let case = super::super::corpus::Manifest::from_toml(
            "[[case]]\nid='x'\nspoken='Hallo'\ntarget='Hallo.'",
        )
        .unwrap()
        .cases
        .remove(0);
        assert_eq!(outcome(&case, "hallo"), ("FAIL", false, true));
    }

    #[test]
    fn path_tolerance_is_explicit_and_only_changes_case() {
        let mut case = super::super::corpus::Manifest::from_toml(
            "[[case]]\nid='path'\nspoken='path'\ntarget='~/Code/Handy'\ncase_insensitive_path=true",
        )
        .unwrap()
        .cases
        .remove(0);
        assert_eq!(outcome(&case, "~/code/handy").0, "SOFT-PASS");
        assert_eq!(outcome(&case, "~/code/handy.").0, "FAIL");
        case.case_insensitive_path = false;
        assert_eq!(outcome(&case, "~/code/handy").0, "FAIL");
        case.needs_llm = true;
        assert_eq!(outcome(&case, "~/code/handy").0, "LLM-dependent");
    }

    #[test]
    fn imported_manifest_preserves_truth_and_provenance() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../bench/corpus/regression");
        let corpus = Corpus::load(&root).unwrap();
        assert_eq!(corpus.manifest.cases.len(), 13);
        assert_eq!(
            corpus.manifest.cases.iter().filter(|c| c.needs_llm).count(),
            1
        );
        assert_eq!(
            corpus
                .manifest
                .cases
                .iter()
                .filter(|c| c.status == CaseStatus::PendingTruth)
                .count(),
            1
        );
        let mut ids = std::collections::HashSet::new();
        for case in corpus.manifest.cases {
            assert!(ids.insert(case.id));
            assert!(!case.spoken.is_empty());
            assert!(case.provenance.is_some());
            assert_eq!(case.variants, ["normal"]);
        }
    }
}
