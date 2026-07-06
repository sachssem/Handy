//! `record`: interactive corpus recording from the default microphone.
//!
//! Walks the manifest and, for each case/variant without a WAV yet, shows the
//! sentence and records from the default mic between two Enter presses, saving a
//! 16 kHz mono WAV to the corpus path. Reuses the app's [`AudioRecorder`], which
//! already resamples capture to 16 kHz mono frames.

use std::io::Write;
use std::path::Path;

use anyhow::{anyhow, Result};

use crate::audio_toolkit::{save_wav_file, AudioRecorder, VadPolicy};

use super::corpus::Corpus;

/// Record missing corpus audio. `only` limits to one case id; `overwrite`
/// re-records even when the WAV already exists.
pub fn record(corpus_root: &Path, only: Option<&str>, overwrite: bool) -> Result<()> {
    let corpus = Corpus::load(corpus_root)?;

    // VAD is bypassed so the whole utterance (including natural pauses) is kept
    // verbatim for scoring.
    let mut recorder = AudioRecorder::new().map_err(|e| anyhow!("recorder init failed: {e}"))?;
    recorder
        .open(None)
        .map_err(|e| anyhow!("failed to open default microphone: {e}"))?;

    println!(
        "Recording corpus at {}. Read each sentence aloud; press Enter to start, Enter to stop.\n\
         Tip: one quiet room, per case do normal / fast / dialect. Ctrl-C to quit anytime.\n",
        corpus.root.display()
    );

    let mut recorded = 0usize;
    let mut skipped = 0usize;

    for case in &corpus.manifest.cases {
        if let Some(filter) = only {
            if case.id != filter {
                continue;
            }
        }

        for variant in &case.variants {
            let path = corpus.audio_path(&case.id, variant);
            if path.exists() && !overwrite {
                skipped += 1;
                continue;
            }

            println!(
                "── {} / {} ──────────────────────────────",
                case.id, variant
            );
            println!("   {}\n", case.spoken);

            prompt("   [Enter] to START recording (or type 's' + Enter to skip): ")?;
            let line = read_line()?;
            if line.trim().eq_ignore_ascii_case("s") {
                println!("   skipped.\n");
                skipped += 1;
                continue;
            }

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
            save_wav_file(&path, &samples).map_err(|e| anyhow!("failed to save WAV: {e}"))?;

            let secs = samples.len() as f64 / 16_000.0;
            println!("   saved {} ({:.1}s)\n", path.display(), secs);
            recorded += 1;
        }
    }

    recorder
        .close()
        .map_err(|e| anyhow!("failed to close recorder: {e}"))?;

    println!("Done. {recorded} recorded, {skipped} skipped.");
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
