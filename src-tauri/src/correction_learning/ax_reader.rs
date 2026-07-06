//! macOS Accessibility read of a target app's focused text field
//! (fork feature: voice-control).
//!
//! Ported from the Phase 0 feasibility spike (`~/tmp/ax_probe`): resolve a
//! *given pid's* app via `AXUIElementCreateApplication`, read its
//! `AXFocusedUIElement`, and return that field's value. The session hands us the
//! pid it snapshotted at paste time, so reads follow the pasted-into app rather
//! than whatever happens to be frontmost later.
//!
//! Hard rule: a secure (password) text field's value is **never** read — such a
//! focus returns [`FocusRead::Secure`] so the caller tears the session down
//! instead of diffing.
//!
//! On non-macOS platforms the reader is a stub returning "no signal", so the
//! learning session degrades silently and the crate still compiles everywhere.
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

/// Outcome of reading a target app's focused field. Lets the session tell a
/// genuine text read apart from a secure field, a dead app, and a transient
/// no-signal tick.
pub enum FocusRead {
    /// A readable text field holding this value.
    Text(String),
    /// A secure (password) field — the value is intentionally never read.
    Secure,
    /// The target app's process is gone (paste target quit).
    AppGone,
    /// No usable text signal this tick (no focused text element, AX error).
    NoSignal,
}

#[cfg(target_os = "macos")]
mod imp {
    use super::FocusRead;
    use core_foundation::base::{CFType, CFTypeRef, TCFType};
    use core_foundation::string::{CFString, CFStringRef};
    use objc2_app_kit::{NSRunningApplication, NSWorkspace};

    type AXUIElementRef = CFTypeRef;
    type AXError = i32;

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateApplication(pid: libc::pid_t) -> AXUIElementRef;
        fn AXUIElementCopyAttributeValue(
            element: AXUIElementRef,
            attribute: CFStringRef,
            value: *mut CFTypeRef,
        ) -> AXError;
    }

    // AX attribute names are plain CFString keys.
    const AX_FOCUSED_UI_ELEMENT: &str = "AXFocusedUIElement";
    const AX_VALUE: &str = "AXValue";
    const AX_ROLE: &str = "AXRole";
    const AX_SUBROLE: &str = "AXSubrole";
    /// Both the role and the subrole of a password field carry this value.
    const SECURE_ROLE: &str = "AXSecureTextField";

    /// Copy an AX attribute value (+1 retained, released when the `CFType`
    /// drops). `None` on any AX error or a null value.
    fn copy_attr(element: AXUIElementRef, attr: &str) -> Option<CFType> {
        let attr_cf = CFString::new(attr);
        let mut value: CFTypeRef = std::ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(element, attr_cf.as_concrete_TypeRef(), &mut value)
        };
        if err != 0 || value.is_null() {
            return None;
        }
        Some(unsafe { CFType::wrap_under_create_rule(value) })
    }

    /// Copy an AX attribute expected to be a string. `None` when absent or not a
    /// `CFString`. Umlauts survive the CFString → Rust conversion intact.
    fn attr_string(element: AXUIElementRef, attr: &str) -> Option<String> {
        copy_attr(element, attr)?
            .downcast_into::<CFString>()
            .map(|s| s.to_string())
    }

    /// pid of the frontmost application via `NSWorkspace`.
    ///
    /// Must be called on the main thread, where `frontmostApplication` is
    /// current (it is refreshed via main-run-loop workspace notifications).
    /// Handy's paste callsite runs on the main thread, so the session snapshots
    /// the pid there and only re-creates the AX element from it afterwards.
    pub fn frontmost_pid() -> Option<i32> {
        let ws = NSWorkspace::sharedWorkspace();
        let app = ws.frontmostApplication()?;
        Some(app.processIdentifier())
    }

    /// A stable identity string for the app with `pid` (bundle id, else the
    /// localized name). Snapshotted alongside the pid so a later read can detect
    /// the pid being recycled by a different process.
    pub fn process_name(pid: i32) -> Option<String> {
        let app = NSRunningApplication::runningApplicationWithProcessIdentifier(pid)?;
        app.bundleIdentifier()
            .or_else(|| app.localizedName())
            .map(|s| s.to_string())
    }

    /// Read the focused text field of the app with `pid`.
    ///
    /// The systemwide focus path fails outside a registered GUI process, so —
    /// exactly as the spike proved — we go through the per-application element
    /// (`AXUIElementCreateApplication(pid)` → `AXFocusedUIElement`), which reads
    /// even when the app is briefly backgrounded.
    ///
    /// `expected_name` is the app identity snapshotted at paste time (if any);
    /// when the pid now resolves to a different app the OS has recycled the
    /// number, so we report [`FocusRead::AppGone`] rather than reading a
    /// stranger's field.
    pub fn read_focused(pid: i32, expected_name: Option<&str>) -> FocusRead {
        // Cheap liveness check: `kill(pid, 0)` failing with ESRCH means the
        // paste target quit, so the session can tear down silently.
        if unsafe { libc::kill(pid, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            return FocusRead::AppGone;
        }

        // PID-reuse guard: if the pid resolves to a different app than the one we
        // snapshotted, the original target quit and its number was reassigned.
        if let Some(expected) = expected_name {
            if let Some(current) = process_name(pid) {
                if current != expected {
                    return FocusRead::AppGone;
                }
            }
        }

        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return FocusRead::NoSignal;
        }
        let app_cf = unsafe { CFType::wrap_under_create_rule(app) };

        let focused = match copy_attr(app_cf.as_concrete_TypeRef(), AX_FOCUSED_UI_ELEMENT) {
            Some(f) => f,
            None => return FocusRead::NoSignal,
        };
        let fref = focused.as_concrete_TypeRef();

        // Secure fields must never be read — check both role and subrole first.
        let role = attr_string(fref, AX_ROLE);
        let subrole = attr_string(fref, AX_SUBROLE);
        if role.as_deref() == Some(SECURE_ROLE) || subrole.as_deref() == Some(SECURE_ROLE) {
            return FocusRead::Secure;
        }

        match attr_string(fref, AX_VALUE) {
            Some(value) => FocusRead::Text(value),
            None => FocusRead::NoSignal,
        }
    }
}

#[cfg(target_os = "macos")]
pub use imp::{frontmost_pid, process_name, read_focused};

/// Stub for platforms without an Accessibility API: no target can be resolved.
#[cfg(not(target_os = "macos"))]
pub fn frontmost_pid() -> Option<i32> {
    None
}

/// Stub for platforms without an Accessibility API: no identity is available.
#[cfg(not(target_os = "macos"))]
pub fn process_name(_pid: i32) -> Option<String> {
    None
}

/// Stub for platforms without an Accessibility API: the field is never readable,
/// so the session never learns anything and degrades silently.
#[cfg(not(target_os = "macos"))]
pub fn read_focused(_pid: i32, _expected_name: Option<&str>) -> FocusRead {
    FocusRead::NoSignal
}
