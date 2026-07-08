//! Run result model plus JSON + human-markdown report writers.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use super::score::{Format, Recognition};

/// One pipeline configuration evaluated in a run (a column of the matrix).
#[derive(Debug, Clone, Serialize)]
pub struct ConfigSpec {
    /// Short label, e.g. `raw` or `rules`.
    pub name: String,
    /// Whether the text-rules layer is applied.
    pub rules: bool,
    /// Whether inverse text normalization is applied (only meaningful with rules).
    pub itn: bool,
}

/// The processed output and format score for one case under one config.
#[derive(Debug, Clone, Serialize)]
pub struct ConfigOutcome {
    pub config: String,
    pub processed: String,
    #[serde(flatten)]
    pub format: Format,
}

/// The full result for one case × variant × model.
#[derive(Debug, Clone, Serialize)]
pub struct CaseResult {
    pub case_id: String,
    pub variant: String,
    pub tags: Vec<String>,
    pub model: String,
    pub spoken: String,
    pub expected: String,
    /// Raw ASR output before any text rules.
    pub raw: String,
    #[serde(flatten)]
    pub recognition: Recognition,
    pub configs: Vec<ConfigOutcome>,
}

/// A complete benchmark run.
#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub timestamp: String,
    pub corpus_root: String,
    pub models: Vec<String>,
    pub configs: Vec<ConfigSpec>,
    pub language: String,
    pub settings_source: String,
    /// Denoise stage applied to the audio before the engine (`none`, `dtln`,
    /// `dtln-mix70`, `dtln-mix50`).
    pub denoise: String,
    pub results: Vec<CaseResult>,
}

/// Mean of an iterator of f64, or `None` when empty.
fn mean<I: IntoIterator<Item = f64>>(it: I) -> Option<f64> {
    let mut sum = 0.0;
    let mut n = 0u64;
    for v in it {
        sum += v;
        n += 1;
    }
    (n > 0).then(|| sum / n as f64)
}

fn fmt_opt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.3}")).unwrap_or_else(|| "-".into())
}

fn fmt_pct(v: Option<f64>) -> String {
    v.map(|x| format!("{:.0}%", x * 100.0))
        .unwrap_or_else(|| "-".into())
}

impl RunReport {
    /// Write `bench-<timestamp>.json` and `bench-<timestamp>.md` into `out_dir`.
    /// Returns `(json_path, md_path)`.
    pub fn write(&self, out_dir: &Path) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("failed to create output dir {}", out_dir.display()))?;

        let json_path = out_dir.join(format!("bench-{}.json", self.timestamp));
        let md_path = out_dir.join(format!("bench-{}.md", self.timestamp));

        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&json_path, json)
            .with_context(|| format!("failed to write {}", json_path.display()))?;

        std::fs::write(&md_path, self.markdown())
            .with_context(|| format!("failed to write {}", md_path.display()))?;

        Ok((json_path, md_path))
    }

    /// Mean format accuracy for a given model+config across all results.
    fn model_config_accuracy(&self, model: &str, config: &str) -> Option<f64> {
        mean(
            self.results
                .iter()
                .filter(|r| r.model == model)
                .flat_map(|r| {
                    r.configs
                        .iter()
                        .filter(|c| c.config == config)
                        .map(|c| c.format.format_accuracy)
                }),
        )
    }

    fn model_config_exact(&self, model: &str, config: &str) -> Option<f64> {
        mean(
            self.results
                .iter()
                .filter(|r| r.model == model)
                .flat_map(|r| {
                    r.configs.iter().filter(|c| c.config == config).map(|c| {
                        if c.format.exact_match {
                            1.0
                        } else {
                            0.0
                        }
                    })
                }),
        )
    }

    /// Render the human-readable markdown report.
    pub fn markdown(&self) -> String {
        let mut s = String::new();
        let _ = writeln!(s, "# Handy voice-dictation benchmark\n");
        let _ = writeln!(s, "- Run: `{}`", self.timestamp);
        let _ = writeln!(s, "- Corpus: `{}`", self.corpus_root);
        let _ = writeln!(s, "- Language: `{}`", self.language);
        let _ = writeln!(s, "- Settings: `{}`", self.settings_source);
        let _ = writeln!(s, "- Denoise: `{}`", self.denoise);
        let _ = writeln!(s, "- Cases scored: {}", self.results.len());
        let cfgs: Vec<String> = self.configs.iter().map(|c| c.name.clone()).collect();
        let _ = writeln!(s, "- Configs: {}\n", cfgs.join(", "));

        // Recognition table (config-independent — scored on raw ASR vs spoken).
        let _ = writeln!(s, "## Recognition accuracy (vs spoken)\n");
        let _ = writeln!(s, "| Model | mean WER | mean CER |");
        let _ = writeln!(s, "| --- | --- | --- |");
        for model in &self.models {
            let wer = mean(
                self.results
                    .iter()
                    .filter(|r| &r.model == model)
                    .map(|r| r.recognition.wer),
            );
            let cer = mean(
                self.results
                    .iter()
                    .filter(|r| &r.model == model)
                    .map(|r| r.recognition.cer),
            );
            let _ = writeln!(s, "| {} | {} | {} |", model, fmt_opt(wer), fmt_opt(cer));
        }
        let _ = writeln!(s);

        // Format-accuracy matrix: model × config.
        let _ = writeln!(s, "## Format accuracy (vs expected)\n");
        let _ = writeln!(s, "Cell = mean format accuracy / exact-match rate.\n");
        let mut header = String::from("| Model |");
        let mut divider = String::from("| --- |");
        for c in &self.configs {
            let _ = write!(header, " {} |", c.name);
            divider.push_str(" --- |");
        }
        let _ = writeln!(s, "{header}");
        let _ = writeln!(s, "{divider}");
        for model in &self.models {
            let mut row = format!("| {model} |");
            for c in &self.configs {
                let acc = self.model_config_accuracy(model, &c.name);
                let exact = self.model_config_exact(model, &c.name);
                let _ = write!(row, " {} / {} |", fmt_opt(acc), fmt_pct(exact));
            }
            let _ = writeln!(s, "{row}");
        }
        let _ = writeln!(s);

        // Per-tag breakdown (format accuracy aggregated across models).
        let _ = writeln!(s, "## Per-tag format accuracy\n");
        let mut tags: BTreeMap<String, ()> = BTreeMap::new();
        for r in &self.results {
            for t in &r.tags {
                tags.insert(t.clone(), ());
            }
        }
        if tags.is_empty() {
            let _ = writeln!(s, "_No tags in corpus._\n");
        } else {
            let mut header = String::from("| Tag |");
            let mut divider = String::from("| --- |");
            for c in &self.configs {
                let _ = write!(header, " {} |", c.name);
                divider.push_str(" --- |");
            }
            let _ = writeln!(s, "{header}");
            let _ = writeln!(s, "{divider}");
            for tag in tags.keys() {
                let mut row = format!("| {tag} |");
                for c in &self.configs {
                    let acc = mean(
                        self.results
                            .iter()
                            .filter(|r| r.tags.iter().any(|t| t == tag))
                            .flat_map(|r| {
                                r.configs
                                    .iter()
                                    .filter(|co| co.config == c.name)
                                    .map(|co| co.format.format_accuracy)
                            }),
                    );
                    let _ = write!(row, " {} |", fmt_opt(acc));
                }
                let _ = writeln!(s, "{row}");
            }
            let _ = writeln!(s);
        }

        // Worst-10 cases under the most-processed config (the last one).
        if let Some(last_cfg) = self.configs.last() {
            let _ = writeln!(s, "## Worst 10 cases (config `{}`)\n", last_cfg.name);
            let mut rows: Vec<(f64, &CaseResult, &ConfigOutcome)> = self
                .results
                .iter()
                .filter_map(|r| {
                    r.configs
                        .iter()
                        .find(|c| c.config == last_cfg.name)
                        .map(|c| (c.format.format_accuracy, r, c))
                })
                .collect();
            rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            for (acc, r, c) in rows.into_iter().take(10) {
                let _ = writeln!(
                    s,
                    "- **{}** / {} / `{}` — acc {:.3}, WER {:.3}",
                    r.case_id, r.variant, r.model, acc, r.recognition.wer
                );
                let _ = writeln!(s, "  - expected: `{}`", one_line(&r.expected));
                let _ = writeln!(s, "  - got:      `{}`", one_line(&c.processed));
                let _ = writeln!(s, "  - raw:      `{}`", one_line(&r.raw));
            }
            let _ = writeln!(s);
        }

        s
    }
}

/// Collapse newlines/tabs so multi-line output stays on one markdown line.
fn one_line(s: &str) -> String {
    s.replace('\n', "\\n").replace('\t', "\\t")
}
