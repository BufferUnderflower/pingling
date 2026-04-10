//! Build script for `core-libbox-windows`.
//!
//! What it does:
//!   1. On non-Windows hosts, exits early — the crate compiles to a
//!      `#[cfg(libbox_stub)]` no-op so the workspace stays portable
//!      and macOS / Linux dev machines don't need a Windows toolchain.
//!   2. On Windows, locates `libbox.dll` + `libbox.h` (gobind output for
//!      sing-box's `libbox` Go package). The default search path is
//!      `core-libbox-windows/frameworks/libbox/`; override with
//!      `PINGLE_LIBBOX_WINDOWS_DIR=/abs/path/to/dir`.
//!   3. Compiles `bridge/libbox_bridge.c` (a thin C shim that turns the
//!      gobind C API into the smaller surface our Rust [`bridge`] module
//!      declares — the same shape as the macOS Obj-C bridge so the two
//!      cores can share `core.rs` design without #cfg-ing every line).
//!   4. Emits link directives so the produced binary links against the
//!      DLL via its `.lib` import library and Windows finds `libbox.dll`
//!      at runtime via the standard DLL search path (alongside the
//!      .exe in the deployed `.app`-equivalent).
//!   5. If the directory can't be found and the `strict` feature is OFF,
//!      builds the stub fallback instead and prints a `cargo:warning`.
//!
//! How to point at the artifacts:
//!   - drop a `libbox/` dir under `frameworks/` containing `libbox.dll`,
//!     `libbox.lib`, and `libbox.h` (the `.lib` is the import library
//!     gobind generates next to the DLL on Windows builds)
//!   - or set `PINGLE_LIBBOX_WINDOWS_DIR=C:\path\to\libbox`
//!
//! Why the stub fallback:
//!   The PoC needs to compile on dev machines that don't have a built
//!   libbox.dll, and on macOS/Linux CI runners that can't even target
//!   Windows. The stub keeps the rest of the workspace building. Production
//!   Windows builds set `--features strict` so a missing DLL becomes a
//!   hard error.

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(libbox_stub)");
    println!("cargo:rerun-if-changed=bridge/libbox_bridge.c");
    println!("cargo:rerun-if-env-changed=PINGLE_LIBBOX_WINDOWS_DIR");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        // Stub mode on non-Windows hosts. The crate still compiles and
        // exposes `LibboxCoreWindows::new()`, every method just returns
        // `VpnError::PrerequisiteMissing("libbox unavailable on this host")`.
        println!("cargo:rustc-cfg=libbox_stub");
        return;
    }

    let crate_root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let libbox_dir = env::var("PINGLE_LIBBOX_WINDOWS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| crate_root.join("frameworks/libbox"));

    let dll = libbox_dir.join("libbox.dll");
    let lib = libbox_dir.join("libbox.lib");
    let header = libbox_dir.join("libbox.h");

    if !libbox_dir.exists() || !dll.exists() || !lib.exists() || !header.exists() {
        let msg = format!(
            "libbox windows artifacts not found at {} (need libbox.dll, \
             libbox.lib, libbox.h) — building stub. Set \
             PINGLE_LIBBOX_WINDOWS_DIR to point at a built libbox dir, \
             or drop the files in. Pass --features strict to make this \
             a hard error.",
            libbox_dir.display()
        );
        if cfg!(feature = "strict") {
            panic!("{msg}");
        }
        println!("cargo:warning={msg}");
        println!("cargo:rustc-cfg=libbox_stub");
        return;
    }

    // Compile the C bridge against the libbox header. The bridge file
    // turns gobind's typedefs (`GoString`, `GoSlice`, `LibboxBoxService`)
    // into the small set of `extern "C"` symbols our Rust side
    // declares in `src/bridge.rs`.
    let mut build = cc::Build::new();
    build
        .file("bridge/libbox_bridge.c")
        .include(&libbox_dir)
        .compile("libboxbridge");

    // Tell cargo where the import library is + name it for linking.
    println!(
        "cargo:rustc-link-search=native={}",
        libbox_dir.display()
    );
    println!("cargo:rustc-link-lib=dylib=libbox");

    // The DLL needs to be alongside the .exe at runtime. We can't copy
    // it from build.rs reliably (the bundle layout depends on the
    // packaging tool), so the install step in tools/windows-bundler
    // (future arc) will mirror libbox.dll into the bundle's bin dir.
    println!(
        "cargo:warning=remember to ship libbox.dll next to the daemon \
         .exe (Windows DLL search path = exe dir first)"
    );
}
