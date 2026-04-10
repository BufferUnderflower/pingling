//! `core-libbox-windows` — sing-box `libbox.dll` driven by Rust on Windows.
//!
//! This crate is the Windows counterpart to [`core-libbox-macos`]
//! (Apple's `Libbox.xcframework` via an Obj-C bridge) and to
//! [`core-singbox-standalone`] (subprocess wrapper around the
//! `sing-box` CLI binary). It calls libbox's gobind C API directly
//! through a thin C shim ([`bridge`]) so the daemon can run sing-box
//! in-process on Windows just as it does on macOS.
//!
//! ## Status: skeleton
//!
//! This is the Stage 1 skeleton. The build script + bridge surface +
//! `VpnCore` plumbing are in place, and the crate compiles cleanly on
//! macOS / Linux dev machines via the [stub fallback](#stub-fallback).
//! What is NOT yet here:
//!
//!   - A real `libbox.dll` build (gobind on Windows targeting MSVC)
//!   - The actual gobind symbol names in `bridge/libbox_bridge.c`
//!     (the shim currently returns "stub" sentinels for every call)
//!   - The Tauri bundler step that copies `libbox.dll` next to the
//!     daemon `.exe` so Windows' DLL search picks it up
//!   - SystemExtension equivalents — Windows uses a Windows Service
//!     for the privileged tunnel side; that's a separate crate
//!     (planned: `service-host-windows`)
//!
//! See `docs/action-plan-windows.md` (TBD) for the rollout sequence.
//!
//! ## Stub fallback
//!
//! When the build script doesn't find `libbox.dll` / `libbox.h` (which
//! is the case on macOS, Linux, and any Windows machine that hasn't
//! yet built libbox), `cfg(libbox_stub)` is set and every method on
//! [`LibboxCoreWindows`] returns
//! `VpnError::PrerequisiteMissing("libbox unavailable on this host")`.
//! The crate still compiles, the workspace still builds, and the
//! daemon falls through to `core-mock` / `core-singbox-standalone`
//! while the integration is being wired in.
//!
//! Set the `strict` Cargo feature to make a missing libbox a hard
//! error instead — used by Windows CI jobs.
//!
//! ## How to point the build at a libbox build
//!
//! Either drop the artifacts under `core-libbox-windows/frameworks/libbox/`:
//!
//! ```text
//! frameworks/libbox/
//! ├── libbox.dll      (the runtime DLL — Windows search path
//! │                    will find it next to the daemon .exe)
//! ├── libbox.lib      (the import library MSVC links against)
//! └── libbox.h        (gobind-emitted header)
//! ```
//!
//! …or set `PINGLE_LIBBOX_WINDOWS_DIR=C:\path\to\libbox` in your shell
//! environment before `cargo build`.
//!
//! ## Why a C shim instead of pure Rust FFI to libbox.h
//!
//! gobind on Windows emits headers full of `GoString`, `GoSlice`, and
//! pointer-to-Go-managed-handle typedefs. Pulling those types into
//! Rust would require either bindgen-generated bindings (which couple
//! the daemon to a particular sing-box gobind ABI version) or a lot
//! of by-hand `extern "C"` blocks that have to track gobind ABI
//! changes whenever sing-box bumps its toolchain. The C shim does the
//! conversion in C — Rust only sees the small set of `extern "C"`
//! functions in [`bridge`], all of which take plain `*const c_char` /
//! `*mut c_void` / `c_int`. Same surface as the macOS Obj-C bridge
//! (`pingle_libbox_*`) so [`core::LibboxCoreWindows`] is structured
//! identically to its macOS sibling.
//!
//! [`core-libbox-macos`]: https://example.invalid/
//! [`core-singbox-standalone`]: https://example.invalid/

mod bridge;
mod core;

pub use crate::core::LibboxCoreWindows;
