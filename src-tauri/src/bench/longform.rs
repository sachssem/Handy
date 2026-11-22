//! `longform`: does a long dictation lose words where it is cut?
//!
//! Joins several recordings into one long clip, cuts it with the app's
//! [`Chunker`] (the cuts a live recording gets) and decodes the chunks twice:
//! on their own and joined with a space, as upstream does, and with the app's
//! [`OverlapDecode`]. The reference is every recording decoded in one run.
//! The report lists the words on both sides of every cut, counts the sentence
//! ends followed by a lowercase word (what a cut inside a sentence leaves) and
//! scores both texts against the reference.

use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use clap::Parser;
use transcribe_cpp::Transcript;

use super::engine::{load_wav_16k, LoadedModel, ModelSpec};
use super::score;
use crate::audio_toolkit::chunking::{join_transcripts, Chunker};
use crate::audio_toolkit::{SileroVad, SpeechScorer, VoiceActivityDetector};
use crate::chunk_overlap::{is_bare, OverlapDecode};

const RATE: f64 = 16_000.0;
/// Words printed on each side of a cut.
const SEAM_WORDS: usize = 8;

#[derive(Parser, Debug)]
pub struct LongformArgs {
    /// Recordings to join, in order.
    #[arg(required = true)]
    pub wavs: Vec<PathBuf>,
    /// Model spec: `[name=][engine:]path` (same grammar as `run --models`).
    #[arg(long)]
    pub model: String,
    /// Base dir for a relative model path (default: the HF hub cache).
    #[arg(long)]
    pub models_dir: Option<PathBuf>,
    /// Language code, or `auto`.
    #[arg(long, default_value = "auto")]
    pub language: String,
    /// The Silero VAD model that scores the pauses.
    #[arg(
        long,
        default_value = "src-tauri/resources/models/silero-vad-v6.2-F32.gguf"
    )]
    pub vad: PathBuf,
    /// Run the app's post-load warm-up inference first.
    #[arg(long)]
    pub warmup: bool,
    /// Comma-separated custom words sent as the vocabulary of every run.
    #[arg(long, value_delimiter = ',')]
    pub vocab: Vec<String>,
    /// Context prompt sent with every run.
    #[arg(long)]
    pub prompt: Option<String>,
    /// Directory for `reference.txt`, `plain.txt` and `overlap.txt`.
    #[arg(long)]
    pub out: Option<PathBuf>,
}

pub(super) fn run(args: LongformArgs) -> Result<()> {
    if !args.vad.exists() {
        bail!("VAD model not found: {} (pass --vad)", args.vad.display());
    }
    let spec = ModelSpec::parse(
        &args.model,
        &args.models_dir.unwrap_or_else(super::default_hf_hub_dir),
    )?;
    let language = (!args.language.eq_ignore_ascii_case("auto")).then_some(args.language.as_str());
    println!("Loading {} from {}", spec.name, spec.path.display());
    let mut model = LoadedModel::load(&spec)?;
    model.set_prompt(args.prompt.clone());
    if args.warmup {
        model.warm_up();
    }
    let mut decode = |audio: &[f32]| -> Result<String> {
        Ok(model.transcribe(audio, language, &args.vocab)?.text)
    };

    let mut joined: Vec<f32> = Vec::new();
    let mut reference: Vec<String> = Vec::new();
    println!("\nReference: each recording in one run");
    for wav in &args.wavs {
        let audio = load_wav_16k(wav)?;
        let text = decode(&audio)?;
        println!(
            "  {:>5.1}s  {:>4} words  {}",
            audio.len() as f64 / RATE,
            text.split_whitespace().count(),
            wav.file_name().unwrap_or_default().to_string_lossy()
        );
        reference.push(text);
        joined.extend_from_slice(&audio);
    }
    let reference = join_transcripts(&reference);
    let chunks = cut_in_child(&args.wavs, &args.vad, &joined)?;
    println!(
        "\n{:.1}s cut into {} chunks",
        joined.len() as f64 / RATE,
        chunks.len()
    );

    // Upstream: every chunk on its own.
    let started = Instant::now();
    let mut plain: Vec<String> = Vec::new();
    for range in &chunks {
        plain.push(decode(&joined[range.clone()])?);
    }
    let plain_secs = started.elapsed().as_secs_f64();

    // The app: every chunk with its neighbours' audio, cut at the seams.
    let started = Instant::now();
    let mut overlap = OverlapDecode::new(&spec.name);
    let mut overlapped: Vec<Transcript> = Vec::new();
    let mut runs = 0;
    for (index, range) in chunks.iter().enumerate() {
        let after = chunks
            .get(index + 1)
            .map_or(&[][..], |next| &joined[next.clone()]);
        let transcript = overlap.decode(
            joined[range.clone()].to_vec(),
            after,
            overlapped.last_mut(),
            |pcm| {
                runs += 1;
                decode(&pcm).map(|text| Transcript {
                    text,
                    ..Default::default()
                })
            },
        )?;
        overlapped.push(transcript);
    }
    let overlap_secs = started.elapsed().as_secs_f64();
    let overlapped: Vec<String> = overlapped.into_iter().map(|t| t.text).collect();

    println!("\nReference");
    report(&reference, std::slice::from_ref(&reference), &reference);
    println!("\nPlain join ({} runs, {plain_secs:.1}s)", chunks.len());
    report(&reference, &plain, &join_transcripts(&plain));
    println!("\nOverlap ({runs} runs, {overlap_secs:.1}s)");
    report(&reference, &overlapped, &join_transcripts(&overlapped));

    if let Some(out) = &args.out {
        std::fs::create_dir_all(out)
            .with_context(|| format!("failed to create {}", out.display()))?;
        // One word per line, so a line diff of two files is a word diff.
        let lines = |text: &str| text.split_whitespace().collect::<Vec<_>>().join("\n") + "\n";
        std::fs::write(out.join("reference.txt"), lines(&reference))?;
        std::fs::write(out.join("plain.txt"), lines(&join_transcripts(&plain)))?;
        std::fs::write(
            out.join("overlap.txt"),
            lines(&join_transcripts(&overlapped)),
        )?;
        println!(
            "\nWrote reference.txt, plain.txt and overlap.txt to {}",
            out.display()
        );
    }
    Ok(())
}

/// Print the words around every cut of `chunks` and the scores of their
/// joined `text`.
fn report(reference: &str, chunks: &[String], text: &str) {
    for pair in chunks.windows(2) {
        let before: Vec<&str> = pair[0].split_whitespace().collect();
        let after: Vec<&str> = pair[1].split_whitespace().collect();
        println!(
            "  … {} | {} …",
            before[before.len().saturating_sub(SEAM_WORDS)..].join(" "),
            after[..after.len().min(SEAM_WORDS)].join(" ")
        );
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    // A sentence end followed by a lowercase word: the mark a cut inside a
    // sentence leaves.
    let cold_starts = words
        .windows(2)
        .filter(|pair| {
            pair[0].ends_with(['.', '?', '!'])
                && pair[1].chars().next().is_some_and(char::is_lowercase)
        })
        .count();
    println!(
        "  {} words, {} sentence ends before a lowercase word, {} chunks without punctuation, WER {:.2}%, CER {:.2}%",
        words.len(),
        cold_starts,
        chunks.iter().filter(|chunk| is_bare(chunk)).count(),
        score::wer(reference, text) * 100.0,
        score::cer(reference, text) * 100.0
    );
}

/// The chunk lengths of the joined recordings, in samples, from a child
/// process. The VAD registers transcribe.cpp's backends CPU-only and that mask
/// is fixed per process, so it cannot share one with a GPU model. The app has
/// the same split: the VAD in its own process, the model in the worker.
fn cut_in_child(wavs: &[PathBuf], vad: &PathBuf, joined: &[f32]) -> Result<Vec<Range<usize>>> {
    let output = std::process::Command::new(std::env::current_exe()?)
        .arg("longform-cuts")
        .arg("--vad")
        .arg(vad)
        .args(wavs)
        .stderr(std::process::Stdio::inherit())
        .output()
        .context("failed to start the cut planner")?;
    if !output.status.success() {
        bail!("the cut planner failed");
    }
    let mut chunks = Vec::new();
    let mut at = 0;
    for line in String::from_utf8(output.stdout)?.lines() {
        let len: usize = line.trim().parse().context("bad cut planner output")?;
        chunks.push(at..at + len);
        at += len;
    }
    if at != joined.len() {
        bail!("the cut planner covered {at} of {} samples", joined.len());
    }
    Ok(chunks)
}

#[derive(Parser, Debug)]
pub struct CutsArgs {
    #[arg(required = true)]
    pub wavs: Vec<PathBuf>,
    #[arg(long)]
    pub vad: PathBuf,
}

/// Print the length in samples of every chunk the app's [`Chunker`] cuts the
/// joined recordings into, one per line.
pub(super) fn cuts(args: CutsArgs) -> Result<()> {
    let mut joined: Vec<f32> = Vec::new();
    for wav in &args.wavs {
        joined.extend_from_slice(&load_wav_16k(wav)?);
    }
    // The chunker only logs a VAD failure and then leaves the audio in one
    // piece, so check the detector here, where the error is visible.
    {
        let mut probe = SileroVad::new(&args.vad, 0.3)?;
        let frames = joined.len() / probe.frame_samples() * probe.frame_samples();
        probe
            .score(&joined[..frames])
            .context("the VAD cannot score this audio")?;
    }
    let vad = args.vad.clone();
    let mut chunker = Chunker::new(Box::new(move || {
        // The threshold only matters for the detector's yes/no answer; the
        // chunker reads the probabilities.
        Ok(Box::new(SileroVad::new(&vad, 0.3)?) as Box<dyn SpeechScorer>)
    }));
    let mut chunks = chunker.push(joined);
    chunks.extend(chunker.finish());
    for chunk in chunks {
        println!("{}", chunk.len());
    }
    Ok(())
}
