// fork(voice-control): builds the self-correction on-device session bridge
// (swift/fork_self_correction.swift) into its own static library, next to
// upstream's Apple Intelligence bridge. Included from build.rs via `#[path]`;
// toolchain detection and swiftc flags mirror `build_apple_intelligence_bridge`
// so both bridges always take the same real/stub decision.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

const REAL_SWIFT_FILE: &str = "swift/fork_self_correction.swift";
const STUB_SWIFT_FILE: &str = "swift/fork_self_correction_stub.swift";
const BRIDGE_HEADER: &str = "swift/fork_self_correction_bridge.h";

pub fn build() {
    println!("cargo:rerun-if-changed={REAL_SWIFT_FILE}");
    println!("cargo:rerun-if-changed={STUB_SWIFT_FILE}");
    println!("cargo:rerun-if-changed={BRIDGE_HEADER}");

    let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR not set"));
    let object_path = out_dir.join("fork_self_correction.o");
    let static_lib_path = out_dir.join("libfork_self_correction.a");

    let sdk_path =
        env::var("SDKROOT").unwrap_or_else(|_| xcrun(&["--sdk", "macosx", "--show-sdk-path"]));
    let swiftc_path = env::var("SWIFTC").unwrap_or_else(|_| xcrun(&["--find", "swiftc"]));

    let framework_path =
        Path::new(&sdk_path).join("System/Library/Frameworks/FoundationModels.framework");
    let force_stub = env::var("HANDY_FORCE_AI_STUB").as_deref() == Ok("1");
    let command_line_tools_only =
        env::var("SWIFTC").is_err() && super::is_command_line_tools_only();
    let source_file = if framework_path.exists() && !force_stub && !command_line_tools_only {
        REAL_SWIFT_FILE
    } else {
        STUB_SWIFT_FILE
    };

    // Same flags as upstream's bridge: library mode (no `_main`), macOS 11
    // deployment target with `@available` runtime checks; FoundationModels is
    // weak-linked by `build_apple_intelligence_bridge`.
    let status = Command::new(&swiftc_path)
        .args([
            "-parse-as-library",
            "-target",
            "arm64-apple-macosx11.0",
            "-sdk",
            &sdk_path,
            "-O",
            "-import-objc-header",
            BRIDGE_HEADER,
            "-c",
            source_file,
            "-o",
            object_path
                .to_str()
                .expect("Failed to convert object path to string"),
        ])
        .status()
        .expect("Failed to invoke swiftc for the self-correction bridge");
    if !status.success() {
        panic!("swiftc failed to compile {source_file}");
    }

    let status = Command::new("libtool")
        .args([
            "-static",
            "-o",
            static_lib_path
                .to_str()
                .expect("Failed to convert static lib path to string"),
            object_path
                .to_str()
                .expect("Failed to convert object path to string"),
        ])
        .status()
        .expect("Failed to create static library for the self-correction bridge");
    if !status.success() {
        panic!("libtool failed for the self-correction bridge");
    }

    // Search paths, Swift runtime rpath and framework links come from the
    // upstream bridge build.
    println!("cargo:rustc-link-lib=static=fork_self_correction");
}

fn xcrun(args: &[&str]) -> String {
    String::from_utf8(
        Command::new("xcrun")
            .args(args)
            .output()
            .expect("Failed to run xcrun")
            .stdout,
    )
    .expect("xcrun output is not valid UTF-8")
    .trim()
    .to_string()
}
