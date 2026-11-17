//! Local transcription socket (fork feature: voice-control).
//!
//! T3 Code (another app on the same Mac) transcribes its composer dictations
//! through the running Handy instead of `handy --transcribe-file`, which would
//! load a second copy of the model. Handy listens on a Unix domain socket at
//! `<app data dir>/transcribe.sock` (mode 0600) and serves each request with
//! its already-loaded model.
//!
//! Protocol: one request per connection. The client sends one JSON line (at
//! most [`MAX_REQUEST_BYTES`]), the server answers one JSON line and closes.
//!
//! - `{"op":"prepare"}` → `{"ok":true,"model":…,"load_ms":…}`: load the
//!   selected model like a dictation press does (warm-up included);
//!   `load_ms` is 0 when it was already loaded.
//! - `{"op":"transcribe","wav":"/abs/path.wav"}` → `{"ok":true,"text":…,
//!   "model":…,"load_ms":…,"transcribe_ms":…,"audio_secs":…}`: 16 kHz mono
//!   16-bit PCM only; runs `TranscriptionManager::transcribe`, the same text
//!   pipeline as a dictation. Journaled as its own `source: "t3"` record.
//! - Any failure → `{"ok":false,"error":…}`.
//!
//! Started from app setup only (never in the headless CLI modes); a bind
//! failure is logged and ignored.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use crate::managers::history::HistoryManager;
use crate::managers::transcription::TranscriptionManager;

const SOCKET_FILE: &str = "transcribe.sock";
/// Longest accepted request line (the newline excluded).
pub(crate) const MAX_REQUEST_BYTES: usize = 64 * 1024;
/// A client that sends nothing for this long is dropped.
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const SAMPLE_RATE: f64 = 16_000.0;

#[derive(Debug, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Prepare,
    Transcribe { wav: PathBuf },
}

#[derive(Debug, PartialEq)]
pub(crate) struct Prepared {
    pub model: String,
    pub load_ms: u64,
}

#[derive(Debug, PartialEq)]
pub(crate) struct Transcribed {
    pub text: String,
    pub model: String,
    pub load_ms: u64,
    pub transcribe_ms: u64,
    pub audio_secs: f64,
}

/// What serves the requests: the running app ([`AppTranscriber`]), or a fake
/// in tests.
pub(crate) trait Transcriber: Send + Sync + 'static {
    fn prepare(&self) -> Result<Prepared, String>;
    fn transcribe(&self, wav: &Path) -> Result<Transcribed, String>;
}

pub(crate) fn parse_request(line: &[u8]) -> Result<Request, String> {
    let request: Request = serde_json::from_slice(line).map_err(|e| format!("bad request: {e}"))?;
    if let Request::Transcribe { wav } = &request {
        if !wav.is_absolute() {
            return Err("bad request: wav must be an absolute path".to_string());
        }
    }
    Ok(request)
}

fn error_reply(error: impl Into<String>) -> Value {
    json!({ "ok": false, "error": error.into() })
}

/// Answer one request line; never panics.
pub(crate) fn handle(line: &[u8], transcriber: &dyn Transcriber) -> Value {
    let request = match parse_request(line) {
        Ok(request) => request,
        Err(e) => return error_reply(e),
    };
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match request {
        Request::Prepare => transcriber
            .prepare()
            .map(|p| json!({ "ok": true, "model": p.model, "load_ms": p.load_ms })),
        Request::Transcribe { wav } => transcriber.transcribe(&wav).map(|t| {
            json!({
                "ok": true,
                "text": t.text,
                "model": t.model,
                "load_ms": t.load_ms,
                "transcribe_ms": t.transcribe_ms,
                "audio_secs": t.audio_secs,
            })
        }),
    }));
    match run {
        Ok(Ok(reply)) => reply,
        Ok(Err(e)) => error_reply(e),
        Err(_) => error_reply("internal error: request handler panicked"),
    }
}

/// Read one request line of at most [`MAX_REQUEST_BYTES`]. A client may close
/// its write side instead of sending the newline.
fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    let mut line = Vec::new();
    BufReader::new(stream.take(MAX_REQUEST_BYTES as u64 + 1))
        .read_until(b'\n', &mut line)
        .map_err(|e| format!("read failed: {e}"))?;
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    if line.len() > MAX_REQUEST_BYTES {
        return Err(format!(
            "request too large (limit {MAX_REQUEST_BYTES} bytes)"
        ));
    }
    Ok(line)
}

fn serve_connection(mut stream: UnixStream, transcriber: &dyn Transcriber) {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let reply = match read_request(&mut stream) {
        Ok(line) => handle(&line, transcriber),
        Err(e) => error_reply(e),
    };
    let mut out = reply.to_string();
    out.push('\n');
    if let Err(e) = stream.write_all(out.as_bytes()) {
        log::debug!("transcribe-socket: reply not delivered: {e}");
    }
}

/// Bind `path` with mode 0600, replacing a stale socket file left by a
/// previous run. Any other file at `path` is left alone and the bind fails.
pub(crate) fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.file_type().is_socket() {
            std::fs::remove_file(path)?;
        }
    }
    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// Accept connections forever, one thread per connection (a transcription
/// can take seconds; requests queue on the engine turn, not here).
pub(crate) fn serve(listener: UnixListener, transcriber: Arc<dyn Transcriber>) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let transcriber = Arc::clone(&transcriber);
                std::thread::spawn(move || serve_connection(stream, transcriber.as_ref()));
            }
            Err(e) => log::warn!("transcribe-socket: accept failed: {e}"),
        }
    }
}

/// Bind the socket and serve it on a background thread.
pub fn start(app: &AppHandle) {
    let path = match crate::portable::app_data_dir(app) {
        Ok(dir) => dir.join(SOCKET_FILE),
        Err(e) => {
            log::warn!("transcribe-socket: no app data dir ({e}); socket disabled");
            return;
        }
    };
    let listener = match bind(&path) {
        Ok(listener) => listener,
        Err(e) => {
            log::warn!(
                "transcribe-socket: cannot bind {}: {e}; socket disabled",
                path.display()
            );
            return;
        }
    };
    log::info!("transcribe-socket: listening on {}", path.display());
    let transcriber: Arc<dyn Transcriber> = Arc::new(AppTranscriber { app: app.clone() });
    std::thread::spawn(move || serve(listener, transcriber));
}

/// Validate (16 kHz mono 16-bit int PCM, what Handy itself records) and read
/// a WAV, like the `--transcribe-file` path.
pub(crate) fn read_pcm_wav(path: &Path) -> Result<Vec<f32>, String> {
    let reader =
        hound::WavReader::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000
        || spec.channels != 1
        || spec.bits_per_sample != 16
        || spec.sample_format != hound::SampleFormat::Int
    {
        return Err(format!(
            "expected 16 kHz mono 16-bit PCM WAV, got {} Hz / {} ch / {}-bit {:?}",
            spec.sample_rate, spec.channels, spec.bits_per_sample, spec.sample_format
        ));
    }
    drop(reader);
    crate::audio_toolkit::read_wav_samples(path)
        .map_err(|e| format!("failed to read {}: {e}", path.display()))
}

/// The running app: Handy's model and transcription pipeline.
struct AppTranscriber {
    app: AppHandle,
}

impl AppTranscriber {
    fn manager(&self) -> Result<Arc<TranscriptionManager>, String> {
        self.app
            .try_state::<Arc<TranscriptionManager>>()
            .map(|tm| Arc::clone(&tm))
            .ok_or_else(|| "transcription manager not ready".to_string())
    }

    /// Load the selected model through the press path (`initiate_model_load`:
    /// pending reloads, the warm-up, the loading flag) and wait for it.
    fn ensure_loaded(&self, tm: &TranscriptionManager) -> Result<Prepared, String> {
        let selected = crate::settings::get_settings(&self.app).selected_model;
        if selected.is_empty() {
            return Err("no model selected in Handy".to_string());
        }
        let started = Instant::now();
        tm.initiate_model_load();
        tm.wait_for_model_load();
        if !tm.is_model_loaded() {
            return Err(format!("failed to load model '{selected}'"));
        }
        Ok(Prepared {
            model: tm.get_current_model().unwrap_or(selected),
            load_ms: started.elapsed().as_millis() as u64,
        })
    }

    /// Copy the request's WAV into Handy's recordings dir as
    /// `t3-<unix secs>.wav` (bumped past an existing file; pruned with the
    /// journal's retention).
    fn keep_wav(&self, wav: &Path, samples: usize) -> Option<(String, bool)> {
        let hm = self.app.try_state::<Arc<HistoryManager>>()?;
        let dir = hm.recordings_dir();
        let now = chrono::Utc::now().timestamp();
        let (file_name, target) = (now..now + 60)
            .map(|ts| {
                let name = crate::journal::t3_wav_name(ts);
                let path = dir.join(&name);
                (name, path)
            })
            .find(|(_, path)| !path.exists())?;
        let saved = std::fs::copy(wav, &target)
            .map_err(|e| e.to_string())
            .and_then(|_| {
                crate::audio_toolkit::verify_wav_file(&target, samples).map_err(|e| e.to_string())
            });
        if let Err(e) = &saved {
            log::warn!("transcribe-socket: keeping WAV failed: {e}");
        }
        Some((file_name, saved.is_ok()))
    }
}

impl Transcriber for AppTranscriber {
    fn prepare(&self) -> Result<Prepared, String> {
        let tm = self.manager()?;
        // A prepare has no record; its load must not land on Handy's.
        crate::journal::without_record(|| self.ensure_loaded(&tm))
    }

    fn transcribe(&self, wav: &Path) -> Result<Transcribed, String> {
        let journal = crate::journal::ExternalDictation::begin("t3");
        let result = journal.scope(|| -> Result<Transcribed, String> {
            let samples = read_pcm_wav(wav)?;
            let audio_secs = samples.len() as f64 / SAMPLE_RATE;
            let sample_count = samples.len();
            let tm = self.manager()?;
            let prepared = self.ensure_loaded(&tm)?;
            if tm.is_streaming() {
                return Err("Handy is running a live dictation; retry when it ends".to_string());
            }
            let started = Instant::now();
            let text = tm.transcribe(samples).map_err(|e| e.to_string());
            let elapsed = started.elapsed();
            let kept = crate::journal::is_enabled()
                .then(|| self.keep_wav(wav, sample_count))
                .flatten();
            journal.recorded(
                audio_secs,
                kept.as_ref().map(|(name, _)| name.as_str()),
                kept.as_ref().map(|(_, saved)| *saved),
            );
            let text = text?;
            journal.transcribed(elapsed, &text);
            Ok(Transcribed {
                text,
                model: prepared.model,
                load_ms: prepared.load_ms,
                transcribe_ms: elapsed.as_millis() as u64,
                audio_secs,
            })
        });
        // Snippet offsets are only read by Handy's own output stages.
        let _ = crate::snippets::take_protection(Some(journal.id()));
        if let Err(e) = &result {
            journal.failed(e);
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Shutdown;
    use std::sync::Mutex;

    #[test]
    fn requests_parse_and_reject_bad_input() {
        assert_eq!(parse_request(br#"{"op":"prepare"}"#), Ok(Request::Prepare));
        assert_eq!(
            parse_request(br#"{"op":"transcribe","wav":"/tmp/a.wav"}"#),
            Ok(Request::Transcribe {
                wav: PathBuf::from("/tmp/a.wav")
            })
        );
        for bad in [
            &br#"{"op":"transcribe","wav":"a.wav"}"#[..],
            br#"{"op":"transcribe"}"#,
            br#"{"op":"delete"}"#,
            br#"{"wav":"/tmp/a.wav"}"#,
            b"not json",
            b"",
        ] {
            assert!(
                parse_request(bad).is_err(),
                "{}",
                String::from_utf8_lossy(bad)
            );
        }
    }

    struct Fake {
        seen: Mutex<Vec<PathBuf>>,
    }

    impl Transcriber for Fake {
        fn prepare(&self) -> Result<Prepared, String> {
            Ok(Prepared {
                model: "fake-model".into(),
                load_ms: 0,
            })
        }

        fn transcribe(&self, wav: &Path) -> Result<Transcribed, String> {
            self.seen.lock().unwrap().push(wav.to_path_buf());
            if wav.ends_with("panic.wav") {
                panic!("engine exploded");
            }
            if wav.ends_with("missing.wav") {
                return Err("cannot open".into());
            }
            Ok(Transcribed {
                text: "Hallo Welt".into(),
                model: "fake-model".into(),
                load_ms: 12,
                transcribe_ms: 34,
                audio_secs: 1.5,
            })
        }
    }

    fn fake() -> Fake {
        Fake {
            seen: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn replies_carry_ok_and_errors() {
        let fake = fake();
        assert_eq!(
            handle(br#"{"op":"prepare"}"#, &fake),
            json!({"ok": true, "model": "fake-model", "load_ms": 0})
        );
        assert_eq!(
            handle(br#"{"op":"transcribe","wav":"/x/a.wav"}"#, &fake),
            json!({"ok": true, "text": "Hallo Welt", "model": "fake-model",
                   "load_ms": 12, "transcribe_ms": 34, "audio_secs": 1.5})
        );
        let missing = handle(br#"{"op":"transcribe","wav":"/x/missing.wav"}"#, &fake);
        assert_eq!(missing, json!({"ok": false, "error": "cannot open"}));
        let panicked = handle(br#"{"op":"transcribe","wav":"/x/panic.wav"}"#, &fake);
        assert_eq!(panicked["ok"], false);
        let unknown = handle(br#"{"op":"nope"}"#, &fake);
        assert_eq!(unknown["ok"], false);
        assert!(unknown["error"]
            .as_str()
            .unwrap()
            .starts_with("bad request"));
    }

    #[test]
    fn wav_validation_rejects_non_handy_formats() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, rate: u32, channels: u16| {
            let path = dir.path().join(name);
            let spec = hound::WavSpec {
                channels,
                sample_rate: rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut writer = hound::WavWriter::create(&path, spec).unwrap();
            for _ in 0..1600 * channels {
                writer.write_sample(0_i16).unwrap();
            }
            writer.finalize().unwrap();
            path
        };
        assert_eq!(
            read_pcm_wav(&write("ok.wav", 16_000, 1)).unwrap().len(),
            1600
        );
        assert!(read_pcm_wav(&write("cd.wav", 44_100, 1)).is_err());
        assert!(read_pcm_wav(&write("stereo.wav", 16_000, 2)).is_err());
        assert!(read_pcm_wav(&dir.path().join("none.wav")).is_err());
    }

    fn request(path: &Path, body: &[u8]) -> Value {
        let mut stream = UnixStream::connect(path).unwrap();
        // The server stops reading an oversized request early.
        let _ = stream.write_all(body);
        let _ = stream.shutdown(Shutdown::Write);
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        assert!(reply.ends_with('\n'), "{reply:?}");
        assert_eq!(reply.matches('\n').count(), 1, "{reply:?}");
        serde_json::from_str(&reply).unwrap()
    }

    #[test]
    fn serves_one_reply_per_connection_over_a_real_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_FILE);
        // A stale socket file from an earlier run is replaced.
        drop(UnixListener::bind(&path).unwrap());
        let listener = bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let fake = Arc::new(fake());
        let transcriber: Arc<dyn Transcriber> = fake.clone();
        std::thread::spawn(move || serve(listener, transcriber));

        assert_eq!(
            request(&path, b"{\"op\":\"prepare\"}\n")["model"],
            "fake-model"
        );
        let reply = request(&path, b"{\"op\":\"transcribe\",\"wav\":\"/x/a.wav\"}\n");
        assert_eq!(reply["ok"], true);
        assert_eq!(reply["text"], "Hallo Welt");
        // Without a trailing newline the closed write side ends the request.
        assert_eq!(request(&path, br#"{"op":"prepare"}"#)["ok"], true);
        assert_eq!(request(&path, b"garbage\n")["ok"], false);

        let oversized = vec![b'x'; MAX_REQUEST_BYTES + 10];
        let reply = request(&path, &oversized);
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("too large"));

        assert_eq!(*fake.seen.lock().unwrap(), [PathBuf::from("/x/a.wav")]);
    }

    #[test]
    fn bind_leaves_a_non_socket_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(SOCKET_FILE);
        std::fs::write(&path, "keep").unwrap();
        assert!(bind(&path).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "keep");
    }
}
