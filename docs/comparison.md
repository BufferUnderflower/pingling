# IPC Transport Options — Tauri Daemon ↔ Flutter

## Context

Pingle uses a **headless Tauri daemon** (no webview) as the native backend. The Flutter
UI needs to communicate with it. Tauri's built-in `invoke()` bridge requires a webview
JS engine — it is not available to Flutter. We need an alternative IPC transport.

This document compares the options and records the decision.

## Options Compared

### Option A: Local Unix socket / named pipe with JSON-RPC 2.0 ✓ (Chosen)

The daemon embeds a JSON-RPC 2.0 server (using `tokio` + `serde_json`) that listens on
a Unix domain socket (`$TMPDIR/pingle.sock`) or Windows named pipe (`\\.\pipe\pingle`).
Flutter connects with a Dart `Socket` client.

```rust
// Daemon side (simplified)
let listener = UnixListener::bind("/tmp/pingle.sock")?;
for stream in listener.incoming() {
    tokio::spawn(handle_jsonrpc(stream?, vpn_manager.clone()));
}

// Flutter side (simplified Dart)
final socket = await Socket.connect(InternetAddress('/tmp/pingle.sock',
    type: InternetAddressType.unix), 0);
socket.write(jsonEncode({'jsonrpc':'2.0','id':1,'method':'vpn.connect','params':{}}));
```

**Push events**: The daemon sends unsolicited notifications to all connected sockets
when state changes (e.g. `event.stateChanged`, `event.coreCrashed`).

| Aspect | Rating |
|--------|--------|
| Tauri version coupling | None — pure Rust std/tokio |
| Language support | Any language (Dart, Swift, Kotlin, Python) |
| Bidirectional push | Yes (server → client notifications) |
| Testability | `nc` or any TCP/socket client works for manual testing |
| Overhead | Near-zero (loopback socket, no serialization overhead) |
| Windows support | Named pipes (same API surface, different path) |

**Verdict**: Best fit. Standard, language-agnostic, easy to test, push-capable.

---

### Option B: Embedded HTTP server (axum / tiny_http)

Spawn an `axum` HTTP server inside the daemon on a random localhost port. Flutter uses
`http` package for requests, SSE or WebSocket for push events. Port stored in a
well-known file or env var.

| Aspect | Rating |
|--------|--------|
| Tauri version coupling | None |
| Language support | Any HTTP client |
| Bidirectional push | SSE or WebSocket (more complex) |
| Testability | `curl` works |
| Overhead | Higher (HTTP headers, port allocation, firewall concerns) |
| Windows support | Yes |

**Verdict**: Works, but higher overhead and firewall/port conflicts are a risk. Prefer
Unix sockets for IPC that stays on-device.

---

### Option C: Tauri `invoke()` bridge (built-in)

Tauri exposes `#[tauri::command]` handlers that JavaScript in a webview calls via
`window.__TAURI__.invoke()`. The webview passes messages through a native bridge.

| Aspect | Rating |
|--------|--------|
| Requires webview | Yes — Flutter cannot use this |
| Flutter support | None |
| Overhead | Low (shared memory bridge) |
| Testability | Requires Tauri test harness |

**Verdict**: Not applicable. Flutter is not a webview. Eliminated.

---

### Option D: Flutter FFI + Rust shared library

Compile the Rust daemon as a `.dylib`/`.so` and load it via Flutter's `dart:ffi`.
Flutter calls Rust functions directly in-process.

| Aspect | Rating |
|--------|--------|
| Tauri dependency | None in the library |
| In-process | Yes (no IPC overhead) |
| System tray | Requires separate Tauri process anyway |
| Complexity | Very high (C ABI, memory management, callback hell) |
| Testability | Hard |

**Verdict**: Viable for mobile (iOS/Android) where the daemon cannot run as a separate
process. Not the right choice for desktop where the daemon must outlive the UI process
and manage system tray independently.

---

### Option E: stdin/stdout pipe (Flutter spawns daemon)

Flutter launches the daemon as a child process and communicates via `stdin`/`stdout`
using newline-delimited JSON.

| Aspect | Rating |
|--------|--------|
| Complexity | Low |
| Daemon lifecycle | Tied to Flutter process — daemon dies when UI closes |
| System tray | Impossible (no independent process) |
| Push events | Works via stdout lines |

**Verdict**: Incompatible with the requirement that the daemon outlives the UI and runs
as a persistent system tray process.

---

## Decision: Option A — Unix socket + JSON-RPC 2.0

**Rationale**:
- The daemon must be an independent process that persists across Flutter app restarts.
- Push events (state changes, log lines, crash notifications) require bidirectional
  communication — a plain request/response model is insufficient.
- JSON-RPC 2.0 is a minimal, well-specified protocol with libraries in every language.
- Unix domain sockets are the lowest-overhead on-device IPC mechanism.
- No Tauri version coupling — the protocol does not change when Tauri upgrades.

**Protocol spec**: [ARCHITECTURE.md — IPC Protocol](../ARCHITECTURE.md#ipc-protocol-tauri-daemon--flutter)

---

## VPN Process Wrapper Options (Historical)

For reference: options for wrapping the sing-box binary inside the daemon.

### Used: `std::process::Command` (core-singbox-standalone)

The daemon spawns sing-box using `std::process::Command`, captures stdout/stderr in
background threads, and uses a reaper thread (`child.try_wait()` at 500 ms intervals)
to detect unexpected exits.

This implementation has **no Tauri dependency** — it works identically from CLI,
the Tauri daemon, or a plain Rust binary. The Tauri app resolves the bundled sidecar
path and passes it to `SingboxStandalone::new(path)`.

### Rejected: Tauri plugin-shell sidecar (direct use as core)

`tauri-plugin-shell` sidecar API is convenient but couples the VPN core to Tauri.
The standalone `std::process::Command` approach is preferred because:

- Works in the CLI binary (no Tauri runtime)
- Testable without Tauri
- Reaper thread provides the same "unexpected exit" detection that plugin-shell's
  `CommandEvent::Terminated` would provide
