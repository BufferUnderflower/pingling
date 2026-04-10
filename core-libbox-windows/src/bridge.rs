//! Rust-side declarations for the C bridge in `bridge/libbox_bridge.c`.
//!
//! Mirrors the macOS Obj-C bridge surface (`core-libbox-macos/src/bridge.rs`)
//! one-to-one so [`crate::core::LibboxCoreWindows`] can keep the same
//! shape as its macOS sibling. The actual symbols below are stubs in
//! the current shim — once `frameworks/libbox/libbox.{dll,lib,h}` is
//! filled in, the C bridge swaps its return values for real
//! `LibboxNewService` / `LibboxBoxService_*` calls and these
//! declarations stay unchanged.
//!
//! ## Stub mode
//!
//! When the build script can't find a libbox build, it sets
//! `cfg(libbox_stub)` and the bridge module is replaced with a stub
//! implementation that doesn't reference the C symbols at all (so the
//! crate still compiles cleanly without the C shim being linked in).
//! Every function returns null / 0 with a "stub" error string.

#![allow(non_camel_case_types)]

use std::os::raw::{c_char, c_int, c_void};

#[cfg(not(libbox_stub))]
extern "C" {
    /// Construct a new libbox service from a config JSON.
    ///
    /// `cfg_json` is a NUL-terminated UTF-8 string. On success returns a
    /// non-NULL opaque handle. On failure returns NULL and writes a
    /// heap-allocated error string into `*err` — caller frees with
    /// [`pingle_libbox_free_string`].
    pub fn pingle_libbox_new_service(cfg_json: *const c_char, err: *mut *mut c_char) -> *mut c_void;

    /// Start the service. Returns 1 on success, 0 on failure (with `*err`
    /// populated). Idempotency is the caller's responsibility.
    pub fn pingle_libbox_service_start(handle: *mut c_void, err: *mut *mut c_char) -> c_int;

    /// Close the service. Same return contract as [`pingle_libbox_service_start`].
    pub fn pingle_libbox_service_close(handle: *mut c_void, err: *mut *mut c_char) -> c_int;

    /// Release the opaque handle. Always safe — passing NULL is a no-op.
    pub fn pingle_libbox_service_release(handle: *mut c_void);

    /// Heap-allocated NUL-terminated version string. Caller frees with
    /// [`pingle_libbox_free_string`]. NULL on failure.
    pub fn pingle_libbox_version() -> *mut c_char;

    /// Free a string previously returned by any of the bridge functions.
    pub fn pingle_libbox_free_string(p: *mut c_char);
}

// ---------------------------------------------------------------------------
// Stub fallback for non-Windows hosts (and Windows hosts that don't yet
// have a built libbox.dll). Mirrors the real signatures so the rest of
// the crate compiles unchanged. Marked dead-code-allowed because in
// stub mode `core.rs` short-circuits before any of these get called.
// ---------------------------------------------------------------------------

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_new_service(
    _cfg_json: *const c_char,
    _err: *mut *mut c_char,
) -> *mut c_void {
    std::ptr::null_mut()
}

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_service_start(_handle: *mut c_void, _err: *mut *mut c_char) -> c_int {
    0
}

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_service_close(_handle: *mut c_void, _err: *mut *mut c_char) -> c_int {
    0
}

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_service_release(_handle: *mut c_void) {}

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_version() -> *mut c_char {
    std::ptr::null_mut()
}

#[cfg(libbox_stub)]
#[allow(dead_code)]
pub unsafe fn pingle_libbox_free_string(_p: *mut c_char) {}
