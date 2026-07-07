//! `record`: interactive corpus recording from the default microphone.
//!
//! Walks the manifest in fixed variant passes and, for each case/variant without
//! a WAV yet, shows the sentence and records from the default mic between two
//! Enter presses, saving a 16 kHz mono WAV to the corpus path. Reuses the app's
//! [`AudioRecorder`], which already resamples capture to 16 kHz mono frames.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, bail, Result};

use crate::audio_toolkit::{save_wav_file, AudioRecorder, VadPolicy};

use super::corpus::Corpus;

const RECORDING_PASSES: [&str; 3] = ["normal", "fast", "noise"];

/// Record corpus audio. By default this records only missing takes; `only`
/// limits to one case id, `overwrite` records all selected takes, and
/// `re_record_specs` targets explicit case ids or case/variant takes.
pub fn record(
    corpus_root: &Path,
    only: Option<&str>,
    overwrite: bool,
    re_record_specs: &[String],
) -> Result<()> {
    let corpus = Corpus::load(corpus_root)?;
    let re_record_specs = parse_re_record_specs(re_record_specs)?;
    validate_filters(&corpus, only, &re_record_specs)?;
    let stats = corpus_stats(&corpus);

    // VAD is bypassed so the whole utterance (including natural pauses) is kept
    // verbatim for scoring.
    let mut recorder = AudioRecorder::new().map_err(|e| anyhow!("recorder init failed: {e}"))?;
    recorder
        .open(None)
        .map_err(|e| anyhow!("failed to open default microphone: {e}"))?;

    println!(
        "Recording corpus at {}. Read each sentence aloud; press Enter to start, Enter to stop.\n\
         Tip: recording runs in passes: all normal takes first, then fast, then noise. Start background music or ambience for the noise pass only. Ctrl-C to quit anytime.\n",
        corpus.root.display()
    );
    println!(
        "Manifest takes: {} total, {} already recorded, {} missing.",
        stats.total, stats.recorded, stats.missing
    );

    let record_all = choose_record_all(&stats, overwrite, &re_record_specs)?;
    let mode = RecordingMode {
        only,
        overwrite: overwrite || record_all,
        re_record_specs: &re_record_specs,
    };

    if !re_record_specs.is_empty() {
        println!(
            "--re-record targets only matching takes; existing files are overwritten after each new take is saved directly to the final path."
        );
    } else if mode.overwrite {
        println!(
            "Re-recording selected takes from scratch; existing files are overwritten after each new take is saved directly to the final path."
        );
    } else if stats.missing == 0 {
        println!(
            "Everything is recorded. Use --re-record <case-id[/variant]> to redo specific takes."
        );
    } else if stats.recorded == 0 {
        println!("No recordings found yet; recording the full corpus.");
    } else {
        println!("Continuing in resume mode; already-recorded takes will be skipped.");
    }
    println!();

    let mut recorded = 0usize;
    let mut skipped = 0usize;

    for (pass_index, variant) in RECORDING_PASSES.iter().enumerate() {
        println!(
            "── Pass {}/{}: {} ──",
            pass_index + 1,
            RECORDING_PASSES.len(),
            variant
        );
        if *variant == "noise" {
            println!(
                "   Start background music or ambience now; you can stop it after this pass.\n"
            );
        }

        for case in &corpus.manifest.cases {
            if !case
                .variants
                .iter()
                .any(|case_variant| case_variant == variant)
            {
                continue;
            }

            let path = corpus.audio_path(&case.id, variant);
            if !mode.should_visit(&case.id, variant, path.exists()) {
                skipped += 1;
                continue;
            }

            if !record_take(&mut recorder, &path, &case.id, variant, &case.spoken)? {
                skipped += 1;
                continue;
            }
            recorded += 1;
        }
    }

    recorder
        .close()
        .map_err(|e| anyhow!("failed to close recorder: {e}"))?;

    println!("Done. {recorded} recorded, {skipped} skipped.");
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ReRecordSpec {
    case_id: String,
    variant: Option<String>,
}

impl ReRecordSpec {
    fn matches(&self, case_id: &str, variant: &str) -> bool {
        self.case_id == case_id
            && self
                .variant
                .as_ref()
                .is_none_or(|spec_variant| spec_variant == variant)
    }
}

struct RecordingMode<'a> {
    only: Option<&'a str>,
    overwrite: bool,
    re_record_specs: &'a [ReRecordSpec],
}

impl RecordingMode<'_> {
    fn should_visit(&self, case_id: &str, variant: &str, exists: bool) -> bool {
        if self.only.is_some_and(|filter| filter != case_id) {
            return false;
        }

        if !self.re_record_specs.is_empty() {
            return self
                .re_record_specs
                .iter()
                .any(|spec| spec.matches(case_id, variant));
        }

        self.overwrite || !exists
    }
}

struct CorpusStats {
    total: usize,
    recorded: usize,
    missing: usize,
}

fn corpus_stats(corpus: &Corpus) -> CorpusStats {
    let mut total = 0usize;
    let mut recorded = 0usize;

    for case in &corpus.manifest.cases {
        for variant in &case.variants {
            total += 1;
            if corpus.audio_path(&case.id, variant).exists() {
                recorded += 1;
            }
        }
    }

    CorpusStats {
        total,
        recorded,
        missing: total.saturating_sub(recorded),
    }
}

fn choose_record_all(
    stats: &CorpusStats,
    overwrite: bool,
    re_record_specs: &[ReRecordSpec],
) -> Result<bool> {
    if overwrite || !re_record_specs.is_empty() || stats.recorded == 0 || stats.missing == 0 {
        return Ok(false);
    }

    prompt("[Enter] continue (record missing only) · type 'all' + Enter to re-record everything from scratch: ")?;
    let line = read_line()?;
    Ok(line.trim().eq_ignore_ascii_case("all"))
}

fn record_take(
    recorder: &mut AudioRecorder,
    path: &Path,
    case_id: &str,
    variant: &str,
    spoken: &str,
) -> Result<bool> {
    println!("── {case_id} / {variant} ──────────────────────────────");
    println!("   {spoken}\n");

    prompt("   [Enter] to START recording (or type 's' + Enter to skip): ")?;
    let line = read_line()?;
    if line.trim().eq_ignore_ascii_case("s") {
        println!("   skipped.\n");
        return Ok(false);
    }

    loop {
        recorder
            .start(VadPolicy::Disabled)
            .map_err(|e| anyhow!("failed to start recording: {e}"))?;
        prompt("   recording... [Enter] to STOP: ")?;
        let _ = read_line()?;
        let samples = recorder
            .stop()
            .map_err(|e| anyhow!("failed to stop recording: {e}"))?;

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        save_wav_file(path, &samples).map_err(|e| anyhow!("failed to save WAV: {e}"))?;

        let secs = samples.len() as f64 / 16_000.0;
        println!("   saved {} ({:.1}s)", path.display(), secs);
        prompt("   [Enter] next · r+Enter re-record this take: ")?;
        let line = read_line()?;
        if !line.trim().eq_ignore_ascii_case("r") {
            println!();
            return Ok(true);
        }
        println!("   re-recording this take.\n");
    }
}

fn parse_re_record_specs(specs: &[String]) -> Result<Vec<ReRecordSpec>> {
    specs
        .iter()
        .map(|spec| parse_re_record_spec(spec))
        .collect()
}

fn parse_re_record_spec(spec: &str) -> Result<ReRecordSpec> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("--re-record cannot be empty");
    }

    let parts: Vec<&str> = spec.split('/').collect();
    match parts.as_slice() {
        [case_id] if !case_id.is_empty() => Ok(ReRecordSpec {
            case_id: (*case_id).to_string(),
            variant: None,
        }),
        [case_id, variant] if !case_id.is_empty() && !variant.is_empty() => Ok(ReRecordSpec {
            case_id: (*case_id).to_string(),
            variant: Some((*variant).to_string()),
        }),
        _ => bail!("invalid --re-record spec `{spec}`; use <case-id> or <case-id>/<variant>"),
    }
}

fn validate_filters(
    corpus: &Corpus,
    only: Option<&str>,
    re_record_specs: &[ReRecordSpec],
) -> Result<()> {
    if let Some(filter) = only {
        if !corpus.manifest.cases.iter().any(|case| case.id == filter) {
            bail!("--only case id `{filter}` is not in the manifest");
        }
    }

    for spec in re_record_specs {
        let Some(case) = corpus
            .manifest
            .cases
            .iter()
            .find(|case| case.id == spec.case_id)
        else {
            bail!(
                "--re-record case id `{}` is not in the manifest",
                spec.case_id
            );
        };

        if let Some(variant) = &spec.variant {
            if !case
                .variants
                .iter()
                .any(|case_variant| case_variant == variant)
            {
                bail!(
                    "--re-record variant `{}` is not listed for case `{}`",
                    variant,
                    spec.case_id
                );
            }
        }
    }

    Ok(())
}

fn prompt(msg: &str) -> Result<()> {
    print!("{msg}");
    std::io::stdout().flush()?;
    Ok(())
}

fn read_line() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf)?;
    Ok(buf)
}
