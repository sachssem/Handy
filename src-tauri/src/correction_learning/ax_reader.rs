//! macOS Accessibility read of a target app's focused text field
//! (fork feature: voice-control).
//!
//! Ported from the Phase 0 feasibility spike (`~/tmp/ax_probe`): resolve a
//! *given pid's* app via `AXUIElementCreateApplication`, read its
//! `AXFocusedUIElement`, and return that field's value. The session hands us the
//! pid it snapshotted at paste time, so reads follow the pasted-into app rather
//! than whatever happens to be frontmost later.
//!
//! It also pins the *element*, not just the app: [`snapshot_focused_element`]
//! captures the AXUIElement that had focus at paste time, and every later read
//! confirms (via `CFEqual`) that the same element still has focus. If the user
//! tabs to a different field in the same app, the read returns
//! [`FocusRead::FocusChanged`] so an unrelated field is never diffed or learned
//! from.
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
    /// Focus moved to a different element than the one snapshotted at paste time
    /// (the user tabbed to another field in the same app). The session ends
    /// rather than diff an unrelated field.
    FocusChanged,
    /// No usable text signal this tick (no focused text element, AX error).
    NoSignal,
}

/// The focused AXUIElement captured at paste time, replayed to [`read_focused`]
/// so every read can confirm the *same* element still has focus. Opaque outside
/// the macOS reader.
#[cfg(target_os = "macos")]
pub use imp::FocusedSnapshot;

/// Stub identity snapshot for platforms without an Accessibility API.
#[cfg(not(target_os = "macos"))]
pub struct FocusedSnapshot;

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

    /// The focused element snapshotted at paste time. Holds the retained
    /// AXUIElement so a later read can `CFEqual`-compare against the currently
    /// focused element and confirm it is the very field that was pasted into.
    ///
    /// `Send` is asserted by hand: the value is created on the paste callsite
    /// (main thread) and moved into the poll thread, where the AX element is
    /// read/compared/released — the same cross-thread AX access the rest of the
    /// reader already performs, and CFRetain/CFRelease/CFEqual are thread-safe.
    pub struct FocusedSnapshot(CFType);

    unsafe impl Send for FocusedSnapshot {}

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

    /// Resolve and retain the app's currently focused AXUIElement, so a later
    /// read can confirm the same element still has focus. Called on the paste
    /// callsite (main thread), right after the text lands in the field. `None`
    /// when nothing is focused or the AX read fails — the caller then skips the
    /// session rather than watch an unattributable field.
    pub fn snapshot_focused_element(pid: i32) -> Option<FocusedSnapshot> {
        let app = unsafe { AXUIElementCreateApplication(pid) };
        if app.is_null() {
            return None;
        }
        let app_cf = unsafe { CFType::wrap_under_create_rule(app) };
        let focused = copy_attr(app_cf.as_concrete_TypeRef(), AX_FOCUSED_UI_ELEMENT)?;
        Some(FocusedSnapshot(focused))
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
    /// stranger's field. `expected_focus` is the element that had focus at paste
    /// time; when focus has since moved to a different element we report
    /// [`FocusRead::FocusChanged`] rather than read an unrelated field.
    pub fn read_focused(
        pid: i32,
        expected_name: Option<&str>,
        expected_focus: &FocusedSnapshot,
    ) -> FocusRead {
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

        // Element-identity gate: the focused element must be the very one we
        // snapshotted at paste time. `CFEqual` on AXUIElements is the documented
        // identity check, so tabbing to another field in the same app (same pid)
        // is caught here and the session ends silently.
        if focused != expected_focus.0 {
            return FocusRead::FocusChanged;
        }

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
pub use imp::{frontmost_pid, process_name, read_focused, snapshot_focused_element};

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

/// Stub for platforms without an Accessibility API: no element can be pinned.
#[cfg(not(target_os = "macos"))]
pub fn snapshot_focused_element(_pid: i32) -> Option<FocusedSnapshot> {
    None
}

/// Stub for platforms without an Accessibility API: the field is never readable,
/// so the session never learns anything and degrades silently.
#[cfg(not(target_os = "macos"))]
pub fn read_focused(
    _pid: i32,
    _expected_name: Option<&str>,
    _expected_focus: &FocusedSnapshot,
) -> FocusRead {
    FocusRead::NoSignal
}
