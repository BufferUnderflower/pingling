# core-libbox-windows

Windows counterpart to [`core-libbox-macos`](../core-libbox-macos/).
Drives sing-box in-process via `libbox.dll` (gobind output) called from
Rust through a thin C shim. Same `VpnCore` lifecycle as the macOS core,
same daemon plumbing on top, just a different bridge underneath.

## Status

**Stage 1 — skeleton.** The build script, the C bridge, the Rust FFI
declarations, and the `VpnCore` impl are all in place and the crate
compiles cleanly on macOS / Linux dev machines via the stub fallback
(`cfg(libbox_stub)`). What is **not** here yet:

- A real `libbox.dll` build (gobind on Windows, MSVC target).
- The actual gobind symbol names in `bridge/libbox_bridge.c` — the shim
  currently returns "stub" sentinels for every call.
- The Tauri / WiX bundler step that copies `libbox.dll` next to the
  daemon `.exe` so Windows' DLL search picks it up at runtime.
- The privileged-side process. macOS uses a System Extension; Windows
  uses a Windows Service. That's a separate crate
  (planned: `service-host-windows`).
- Live capability discovery (`WinTun` driver presence, admin rights,
  Defender exclusions, etc.) in `check_prerequisites`.

See [`docs/action-plan-windows.md`](../docs/action-plan-windows.md) for
the rollout sequence.

## Stub fallback

When the build script can't find `libbox.dll` / `libbox.h` (the
default state on macOS, Linux, and any Windows machine that hasn't yet
built libbox), `cfg(libbox_stub)` is set and every method on
`LibboxCoreWindows` returns `VpnError::PrerequisiteMissing("libbox
unavailable on this host")`. The crate still compiles, the workspace
still builds, and the daemon falls through to `core-mock` /
`core-singbox-standalone` while the integration is being wired in.

Set the `strict` Cargo feature to make a missing libbox a hard error
instead — used by the future Windows CI job.

## Pointing the build at a libbox build

Either drop the artifacts under `core-libbox-windows/frameworks/libbox/`:

```text
frameworks/libbox/
├── libbox.dll      runtime DLL — Windows DLL search will find it
│                    next to the daemon .exe at startup
├── libbox.lib      import library MSVC links against
└── libbox.h        gobind-emitted header
```

…or set `PINGLE_LIBBOX_WINDOWS_DIR=C:\path\to\libbox` in your shell
environment before `cargo build`.

## Why a C shim instead of pure Rust FFI

`gobind` on Windows emits headers full of `GoString`, `GoSlice`, and
pointer-to-Go-managed-handle typedefs. Pulling those types into Rust
would require either bindgen-generated bindings (which couple the
daemon to a particular sing-box gobind ABI version) or a lot of
hand-written `extern "C"` blocks that have to track gobind ABI changes
whenever sing-box bumps its toolchain.

The C shim does the conversion in C, so the Rust side only sees the
small set of `extern "C"` functions in `src/bridge.rs`, all of which
take plain `*const c_char` / `*mut c_void` / `c_int`. The same surface
as the macOS Obj-C bridge (`pingle_libbox_*`) — `core.rs` is structured
identically to its macOS sibling and a future refactor could share
half the file via a `cfg(target_os)` switch on the bridge module path.

## Why this and not the tunnet approach

The reference example we looked at (libbox-on-windows-via-rust in the
tunnet repo) wired Go's runtime + a TUN device + sing-box's
configuration loader and the FFI surface all in one crate, with no
clear separation between "embed Go" and "implement VpnCore". The idea
of using libbox via Rust on Windows is sound; the architectural
choice of mashing it all into one crate is a smell — we want the
embed-Go bit to be a single thin crate (this one) that exposes the
exact same `VpnCore` trait as everything else, and the rest of the
stack (privileged service, TUN management, netwatcher, config
processor) to live in their own crates with their own contracts.

## Local dev

```sh
# Build the stub on macOS / Linux (works out of the box):
cargo build -p core-libbox-windows

# Run the stub-only unit tests (covered by `cargo test --workspace`):
cargo test -p core-libbox-windows

# Build for real on Windows (requires libbox.dll/.lib/.h staged):
set PINGLE_LIBBOX_WINDOWS_DIR=C:\path\to\libbox
cargo build -p core-libbox-windows --target x86_64-pc-windows-msvc
```

Cross-compiling from macOS targeting Windows is possible with
`cargo-xwin` but is not yet wired into CI; for now Windows builds
should run on a real Windows host or a Windows GitHub Actions runner.
