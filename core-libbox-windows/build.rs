//! Build script for `core-libbox-windows`.
//!
//! The Rust side declares `pingle_libbox_*` as `extern "C"` symbols in
//! [`src/bridge.rs`]. The actual implementations live in `libbox.dll`,
//! which is a c-shared Go library produced from the SagerNet/sing-box
//! `experimental/libbox` package (private shim in
//! `pingle-daemon/tools/libbox-windows/`). The shim's `//export`
//! directives emit the exact symbol names Rust imports, so there is
//! **no C translation layer** — the linker resolves the names directly
//! from the DLL.
//!
//! What this script does:
//!
//! 1. On non-Windows hosts, sets `cfg(libbox_stub)` and exits. The
//!    crate compiles to a no-op that returns `PrerequisiteMissing` for
//!    every method, keeping macOS/Linux dev builds of the workspace
//!    clean.
//! 2. On Windows targets (both `x86_64-pc-windows-msvc` and
//!    `x86_64-pc-windows-gnu`), locates the libbox artifacts, validates
//!    them, and emits the right `rustc-link-*` directives for the
//!    toolchain family.
//!
//! ## Artifact search path
//!
//! Looks, in order:
//!
//! 1. `$PINGLE_LIBBOX_WINDOWS_DIR` (absolute path) if set in the build env.
//! 2. `core-libbox-windows/frameworks/libbox/` relative to the crate root.
//!
//! The dir must contain `libbox.dll` and `libbox.h`, plus an import
//! library matching the Windows toolchain family:
//!   - MSVC (`x86_64-pc-windows-msvc`): `libbox.lib` (COFF)
//!   - GNU  (`x86_64-pc-windows-gnu`):  `libbox.dll.a` (MinGW)
//!
//! Both are produced by the `build:libbox-windows` GitLab CI job in
//! pingle-daemon and shipped together in the libbox artifact bundle.
//!
//! ## Stub fallback
//!
//! If the artifacts can't be found and the `strict` feature is off,
//! the build falls back to the stub path (`cfg(libbox_stub)`) with a
//! `cargo:warning`. In CI we pass `--features strict` so missing
//! artifacts break the build loudly.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(libbox_stub)");
    println!("cargo:rerun-if-env-changed=PINGLE_LIBBOX_WINDOWS_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        // Stub mode: every LibboxCoreWindows method returns
        // PrerequisiteMissing. The bridge module compiles with
        // cfg(libbox_stub) no-op bodies, so no extern symbols are
        // referenced.
        println!("cargo:rustc-cfg=libbox_stub");
        return;
    }

    let crate_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libbox_dir = env::var("PINGLE_LIBBOX_WINDOWS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate_root.join("frameworks/libbox"));

    // target_env distinguishes "gnu" (mingw ld) from "msvc" (link.exe).
    // The two families need different linker directives.
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();

    let dll = libbox_dir.join("libbox.dll");
    let header = libbox_dir.join("libbox.h");
    // Each Windows toolchain family needs its own import library flavor.
    let import_lib_name = match target_env.as_str() {
        "msvc" => "libbox.lib",
        _ => "libbox.dll.a",
    };
    let import_lib = libbox_dir.join(import_lib_name);

    let mut missing: Vec<String> = Vec::new();
    if !dll.exists() {
        missing.push(dll.display().to_string());
    }
    if !header.exists() {
        missing.push(header.display().to_string());
    }
    if !import_lib.exists() {
        missing.push(import_lib.display().to_string());
    }

    if !libbox_dir.exists() || !missing.is_empty() {
        let msg = format!(
            "libbox windows artifacts missing in {}: [{}]. Set \
             PINGLE_LIBBOX_WINDOWS_DIR or drop the files into \
             frameworks/libbox/. Pass --features strict to make this a \
             hard error.",
            libbox_dir.display(),
            missing.join(", ")
        );
        if cfg!(feature = "strict") {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        println!("cargo:rustc-cfg=libbox_stub");
        return;
    }

    // Everything present — emit link directives. Uniform across MSVC
    // and GNU: `rustc-link-lib=dylib=libbox` tells rustc to link
    // against an import library named `libbox`. link.exe resolves it
    // to libbox.lib, ld resolves it to libbox.dll.a — both sit in the
    // search dir below, so the same directive handles both toolchains.
    println!("cargo:rerun-if-changed={}", dll.display());
    println!("cargo:rustc-link-search=native={}", libbox_dir.display());
    println!("cargo:rustc-link-lib=dylib=libbox");

    // Reminder: the .exe won't run without libbox.dll adjacent to it.
    // Bundling is the installer's job (WiX / our windows-bundler).
    println!(
        "cargo:warning=remember to ship libbox.dll next to the daemon \
         .exe (Windows DLL search path = exe dir first)"
    );
}
