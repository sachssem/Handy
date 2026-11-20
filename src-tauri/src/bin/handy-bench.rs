//! `handy-bench` — headless voice-dictation benchmark harness for the
//! voice-control fork. See `bench/README.md` for usage.
//!
//! This is a thin entry point; all logic lives in `handy_app_lib::bench` so it
//! can reach the crate-internal engine loading, text-rules and settings code
//! without standing up a Tauri `AppHandle`.

fn main() {
    if let Err(err) = handy_app_lib::bench::run() {
        eprintln!("handy-bench: {err:#}");
        std::process::exit(1);
    }
}
