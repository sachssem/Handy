//! `export-history`: turn real transcription history + recordings into corpus
//! case stubs, so real-world dictation failures become benchmark cases.
//!
//! Reads `<app_data>/history.db` and `<app_data>/recordings/`, copies each
//! recording into the corpus, and writes a `history-stubs.toml` fragment the
//! user reviews and pastes into `manifest.toml`. It never touches the committed
//! `manifest.toml`.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;

use super::corpus::{Case, Manifest};

/// Export the most recent `limit` history rows (or all) from `app_data` into the
/// corpus at `out`.
pub fn export(app_data: &Path, out: &Path, limit: Option<usize>) -> Result<()> {
    let db_path = app_data.join("history.db");
    let recordings = app_data.join("recordings");

    let conn = Connection::open(&db_path)
        .with_context(|| format!("failed to open history db {}", db_path.display()))?;

    let sql = "SELECT id, file_name, transcription_text \
               FROM transcription_history ORDER BY timestamp DESC";
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    std::fs::create_dir_all(out)
        .with_context(|| format!("failed to create corpus dir {}", out.display()))?;

    let mut cases = Vec::new();
    let mut copied = 0usize;
    let mut missing_audio = 0usize;

    for (id, file_name, text) in rows.into_iter().take(limit.unwrap_or(usize::MAX)) {
        let case_id = format!("hist-{id}");
        let src = recordings.join(&file_name);
        if src.exists() {
            let case_dir = out.join(&case_id);
            std::fs::create_dir_all(&case_dir)?;
            std::fs::copy(&src, case_dir.join("normal.wav")).with_context(|| {
                format!("failed to copy recording {} for {}", src.display(), case_id)
            })?;
            copied += 1;
        } else {
            missing_audio += 1;
            eprintln!("  no recording on disk for {case_id} ({file_name}); stub still emitted");
        }

        cases.push(Case {
            id: case_id,
            spoken: text,
            expected: String::new(),
            tags: vec!["history".to_string()],
            variants: vec!["normal".to_string()],
            notes: Some(
                "TODO verify: `spoken` is the raw ASR transcription — correct it, then fill \
                 `expected` with the intended formatted output."
                    .to_string(),
            ),
        });
    }

    let manifest = Manifest {
        cases: cases.clone(),
    };
    let body = manifest.to_toml()?;
    let stub_path = out.join("history-stubs.toml");
    let header = "# handy-bench export-history stubs — REVIEW BEFORE MERGING into manifest.toml.\n\
                  # `spoken` is the raw ASR transcription (marked TODO verify); `expected` is empty.\n\n";
    std::fs::write(&stub_path, format!("{header}{body}"))
        .with_context(|| format!("failed to write {}", stub_path.display()))?;

    println!(
        "Exported {} stub case(s) ({} WAV(s) copied, {} without audio) to {}",
        cases.len(),
        copied,
        missing_audio,
        stub_path.display()
    );
    Ok(())
}
