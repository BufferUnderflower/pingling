# Pingle — Architecture Overview

## Vision

**Pingle** is a cross-platform VPN client whose UI is a native Flutter application on each
platform (macOS, Linux, Windows, iOS, Android), backed by a Rust/Tauri daemon that owns all
VPN lifecycle logic. The Tauri process has **no webview** — it runs headless as a system tray
daemon and exposes a typed IPC surface over a local Unix domain socket (or named pipe on
Windows). Flutter connects to this socket and drives the daemon through a simple JSON-RPC
protocol.

This architecture was chosen because:

- Flutter produces better-looking, more consistent native UI than a Tauri webview, with full
  access to platform-specific rendering (Material, Cupertino) and animations.
- Rust/Tauri owns process lifecycle, privilege escalation, sidecar management and OS
  integration — things Flutter cannot do well natively on desktop.
- The separation is clean: Flutter is a display and interaction layer only; all business logic
  and state lives in the Rust daemon.

---

## System Map

```
┌─────────────────────────────────────────────────────────────────────────┐
│  Flutter App (per platform)                                             │
│  ─────────────────────────────────────────────────────────────────────  │
│  UI widgets · state management (Riverpod / BLoC)                        │
│  Dart IPC client (JSON-RPC over Unix socket / named pipe)               │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │  Unix domain socket / named pipe
                                   │  JSON-RPC 2.0 (request/response + push events)
┌──────────────────────────────────▼──────────────────────────────────────┐
│  Tauri Daemon (pingle — this repo)                                       │
│  ─────────────────────────────────────────────────────────────────────  │
│  IPC server · system tray · sidecar management · auto-update           │
│  ┌──────────────────────────────────────────────────────────────────┐  │
│  │  service — VpnManager                                            │  │
│  │  CoreRegistry · SettingsStorage · typed Pipeline<Op> fields      │  │
│  │  middleware: LoggingMiddleware, GeoFilterMiddleware, …            │  │
│  │  handlers: ConnectHandler, DisconnectHandler, …                  │  │
│  └─────────────────────────────┬────────────────────────────────────┘  │
│                                │                                        │
│  ┌─────────────────────────────▼────────────────────────────────────┐  │
│  │  domain — zero-dependency contracts                              │  │
│  │  VpnCore trait · Pipeline/Middleware/Handler · Operation types   │  │
│  └─────────────────────────────┬────────────────────────────────────┘  │
│                                │                                        │
│  ┌─────────────────────────────▼────────────────────────────────────┐  │
│  │  core-singbox-standalone                                         │  │
│  │  std::process::Command wrapper around the sing-box binary        │  │
│  └──────────────────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────┘
                                   │  spawns
┌──────────────────────────────────▼──────────────────────────────────────┐
│  sing-box (or xray, …) — bundled sidecar binary                        │
│  run -c <config> — owns the actual VPN tunnel                          │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## Domains and Responsibilities

### 1. `domain` — Pure Contracts (zero dependencies)

**Responsibility**: Define the shared language of the system. No I/O, no async, no
serialization.

| Item | Purpose |
|------|---------|
| `VpnCore` trait | Lifecycle contract: `start`, `stop`, `kill`, `restart`, `status`, `running`, `validate_config`, `check_prerequisites`, `subscribe` |
| `SettingsStorage` trait | Key/value persistence contract |
| `ConnectionState` | Enum: Disconnected / Connecting / Connected / Disconnecting / Error |
| `CoreEvent` | Events emitted by a running core: Started, Stopped, Log, ErrorLog, StateChanged, Crashed |
| `CoreDescriptor` | Metadata about a registered core (type, binary path, availability) |
| `VpnError` | Unified error type for the whole system |
| `Operation` trait | Typed operation with `Input`/`Output` associated types |
| `Handler<Op>` trait | Terminal handler for an operation (the core logic) |
| `Middleware<Op>` trait | Composable interceptor with priority ordering |
| `Pipeline<Op>` | Chain of middleware wrapping a terminal handler |
| `ops::*` | Typed operations: `OpConnect`, `OpDisconnect`, `OpRestart`, `OpValidateConfig`, `OpGetStatus`, `OpListOutbounds`, `OpSelectOutbound`, `OpTestLatency` |

The pipeline system is inspired by [tower](https://docs.rs/tower) and
[Envoy filter chains](https://www.envoyproxy.io/docs/envoy/latest/intro/arch_overview/http/http_filters)
-- adapted for synchronous Rust with zero external dependencies.

Operations are split into two groups:

- **Lifecycle** (every core): OpConnect, OpDisconnect, OpRestart, OpValidateConfig, OpGetStatus
- **Capability** (optional): OpListOutbounds, OpSelectOutbound, OpTestLatency

The presence or absence of a pipeline for a capability operation IS the capability
declaration. No stringly-typed capability sets needed.

**Acceptance criteria**: No `extern crate` other than `std`. Compiles in `no_std`-adjacent
environments. 100% of public items have doc-comments.

---

### 2. `data` — Storage Implementations

**Responsibility**: Implement `SettingsStorage` for different backends.

| Item | Purpose |
|------|---------|
| `MemorySettingsStorage` | In-process hash map — used in tests and CLI mode |
| `TauriStoreSettings` | Backed by `tauri-plugin-store` (JSON file, atomic writes) — used by the daemon |

**Acceptance criteria**: Both impls pass the same test suite via a shared test helper.
`MemorySettingsStorage` has zero Tauri dependency.

---

### 3. `config` — Multi-Source Configuration

**Responsibility**: Load and merge `PinglingConfig` from YAML files, JSON files, and
environment variables. Provides `ConfigLoader` with a priority cascade:
env vars > explicit file > default search paths.

**Acceptance criteria**: `ConfigLoader::from_env()` works in CI with no files present
(pure env var config). All merge priority rules are covered by tests.

---

### 4. `core-singbox-standalone` — VPN Engine Adapter

**Responsibility**: Implement `VpnCore` for the sing-box binary using
`std::process::Command`. Owns the child process handle, stdout/stderr capture threads,
a reaper thread that detects unexpected exits, and config validation via
`sing-box check -c`.

No Tauri dependency. Works identically from CLI, headless daemon, or Tauri app.

**Acceptance criteria**: Full lifecycle (start → running → external kill → Crashed event →
state = Disconnected) covered by tests using `/bin/sleep` as a stand-in binary.

---

### 5. `service` — Orchestration Layer

**Responsibility**: `VpnManager` wires a `CoreRegistry` (selected `VpnCore`) with a
`SettingsStorage` and typed `Pipeline<Op>` fields. Exposes a high-level API consumed by
both the CLI and the IPC server:

```
connect() · disconnect() · restart() · force_kill()
get_status() · is_running()
list_cores() · active_core_type() · switch_core()
get_setting() · set_setting() · remove_setting()
capabilities() — returns which optional pipelines are registered
```

VpnManager holds a `Pipeline<Op>` for each lifecycle operation and `Option<Pipeline<Op>>`
for each capability operation. Capability pipelines are registered at construction time
via `set_list_outbounds()`, `set_select_outbound()`, etc. The `capabilities()` method
returns which optional pipelines exist.

Middleware registration uses `connect_pipeline().push(...)` and similar accessors.

Built-in middleware (in `service/src/middleware/`):

| Middleware | Operation | Purpose |
|------------|-----------|---------|
| `LoggingMiddleware` | all | Logs input/output at priority 0 (outermost) |
| `GeoFilterMiddleware` | OpListOutbounds | Filters outbounds by country whitelist |
| `LatencyBiasMiddleware` | OpTestLatency | Adjusts latency measurements |
| `SingboxConfigHandler` | OpListOutbounds | Parses sing-box config to extract outbounds |

Terminal handlers (in `service/src/handlers.rs`): `ConnectHandler`, `DisconnectHandler`,
`RestartHandler`, `ValidateConfigHandler`, `GetStatusHandler`.

**Acceptance criteria**: All methods are covered by unit tests using `MockVpnCore` and
`MemorySettingsStorage`. Mutex poison does not propagate (all locks use
`unwrap_or_else(|e| e.into_inner())`).

---

### 6. `app` — Tauri Daemon (IPC Server + System Tray)

**Responsibility**: The top-level binary. Runs headless (no webview window open by default).
Owns:

- **IPC server**: A local Unix domain socket (or Windows named pipe) running a JSON-RPC 2.0
  handler. Flutter connects here.
- **System tray**: Status icon (red/yellow/green), core selector, config picker, connect /
  disconnect / restart buttons. Updates within 500 ms of any state change via background
  poll.
- **Sidecar management**: Bundled sing-box binary resolved via Tauri's sidecar mechanism.
- **State machine bridge**: All Tauri IPC `#[tauri::command]` handlers delegate directly to
  `VpnManager`.

**What the app does NOT own**: UI, routing, user preferences UI, animations — all Flutter.

**Acceptance criteria**:
- Flutter Dart client can connect to the socket, call `vpn.connect`, and receive a
  `StateChanged` push event within 1 second.
- System tray icon matches actual connection state within 500 ms of any change (including
  external process death).
- `app` binary starts without a visible window on all three desktop platforms.

---

### 7. `cli` — Headless CLI Binary

**Responsibility**: A standalone `clap`-based binary for scripting, CI, and debugging.
Wires `ConfigLoader → CoreRegistry → VpnManager` without Tauri and exposes subcommands:
`start`, `stop`, `status`, `restart`, `validate`, `info`, `prereqs`.

**Acceptance criteria**: All subcommands return non-zero exit codes on error. `status`
outputs machine-readable JSON when `--json` is passed.

---

## IPC Protocol (Tauri Daemon ↔ Flutter)

### Transport

| Platform | Transport |
|----------|-----------|
| macOS / Linux | Unix domain socket at `$XDG_RUNTIME_DIR/pingle.sock` or `$TMPDIR/pingle.sock` |
| Windows | Named pipe `\\.\pipe\pingle` |

The socket is created by the Tauri daemon on startup and deleted on clean shutdown.
Flutter detects the socket path via a well-known location or a sidecar launcher script.

### Protocol: JSON-RPC 2.0

**Requests** (Flutter → Daemon):

```json
{ "jsonrpc": "2.0", "id": 1, "method": "vpn.connect", "params": {} }
{ "jsonrpc": "2.0", "id": 2, "method": "vpn.status", "params": {} }
{ "jsonrpc": "2.0", "id": 3, "method": "vpn.disconnect", "params": {} }
{ "jsonrpc": "2.0", "id": 4, "method": "config.setPath", "params": { "path": "/etc/pingle/config.json" } }
{ "jsonrpc": "2.0", "id": 5, "method": "core.list", "params": {} }
{ "jsonrpc": "2.0", "id": 6, "method": "core.switch", "params": { "coreType": "sing-box" } }
```

**Push events** (Daemon → Flutter, no `id`):

```json
{ "jsonrpc": "2.0", "method": "event.stateChanged", "params": { "state": "Connected", "core": "sing-box" } }
{ "jsonrpc": "2.0", "method": "event.coreLog", "params": { "level": "info", "message": "..." } }
{ "jsonrpc": "2.0", "method": "event.coreCrashed", "params": { "code": -1, "reason": "exited unexpectedly" } }
```

### Why JSON-RPC 2.0 over a socket (not Tauri's built-in JS↔Rust IPC)

Tauri's built-in `invoke()` bridge uses a webview's JavaScript engine as the transport —
it cannot be used by Flutter. A local socket with a standard protocol is:

- Language-agnostic: any Dart, Swift, or Kotlin client can speak it
- Tauri-version-agnostic: the protocol does not change when Tauri upgrades
- Testable independently: `nc` or a simple Rust test client can exercise the daemon
- Future-proof: a mobile companion app (iOS/Android) can talk to a desktop daemon over
  LAN using the same protocol with minimal changes

---

## Features by Layer

| Feature | Layer |
|---------|-------|
| VPN process lifecycle (start/stop/restart/kill) | core-singbox-standalone |
| Config validation (`sing-box check`) | core-singbox-standalone |
| Multi-source config (YAML/JSON/env) | config |
| Settings persistence | data (TauriStoreSettings) |
| Orchestration + typed middleware pipelines | service |
| Pipeline composition (Operation/Handler/Middleware) | domain |
| Typed operations (OpConnect, OpListOutbounds, ...) | domain/src/ops/ |
| Capability declaration (Option<Pipeline<Op>>) | service |
| IPC server (JSON-RPC over socket) | app |
| System tray + status icons | app |
| Sidecar binary bundling | app (tauri.conf.json) |
| Flutter UI, navigation, animations | Flutter app (separate repo) |
| Dart IPC client | Flutter app (separate repo) |
| iOS / Android companion | Flutter app (separate repo, future) |

---

## Goals and Acceptance Criteria

### G1 — Daemon Stability

**Goal**: The Rust daemon must never crash due to panics from lock poisoning, external
process death, or bad IPC input.

**Acceptance criteria**:
- All `Mutex::lock()` calls use `unwrap_or_else(|e| e.into_inner())`.
- External sing-box death (SIGKILL) is detected within 500 ms by the reaper thread;
  state transitions to `Disconnected` and a `CoreEvent::Crashed` is emitted.
- Malformed JSON-RPC input returns a JSON-RPC error response, not a panic.

---

### G2 — Flutter Connectivity

**Goal**: A Flutter client can connect to the daemon, issue commands, and receive push
events without any Tauri webview involved.

**Acceptance criteria**:
- Integration test: a Dart script connects to the socket, calls `vpn.connect`, and
  receives `event.stateChanged` with `"state": "Connected"` within 2 seconds.
- Connection is re-established automatically if Flutter is restarted while the daemon
  continues running.
- The daemon does not exit when Flutter disconnects.

---

### G3 — System Tray Accuracy

**Goal**: The tray icon always reflects actual VPN state within 500 ms.

**Acceptance criteria**:
- Icon is green when sing-box is running and the process is alive.
- Icon turns red within 500 ms of an external kill.
- Icon is yellow during the Connecting / Disconnecting transient states.
- Menu buttons are enabled/disabled correctly (Connect disabled when running,
  Disconnect enabled only when running).

---

### G4 — Multi-Core Plug-in

**Goal**: A new VPN engine (e.g. xray) can be added by implementing `VpnCore` in a new
crate with zero changes to `service`, `data`, `domain`, or `app`. Capability pipelines
(list outbounds, select outbound, test latency) are registered only if the core supports
them -- the presence of the pipeline IS the capability declaration.

**Acceptance criteria**:
- `core-xray-standalone` crate added: `cargo test -p core-xray-standalone` passes.
- `app/src/main.rs` registers the new core via `registry.register(...)` under a feature
  flag.
- Core-specific capability handlers are registered via `set_list_outbounds()` etc.
- The Flutter core-selector menu shows the new core automatically via `core.list`.
- `capabilities()` accurately reports which optional pipelines exist.

---

### G5 — Headless / CI Operation

**Goal**: The full VPN lifecycle works without a display (no tray, no window) for
automation and CI.

**Acceptance criteria**:
- `cargo run -p cli -- start --config test.json` starts sing-box, exits 0.
- `cargo run -p cli -- status` prints `{"state":"Connected"}` without a GUI.
- The daemon can be launched with `AVARS_NO_TRAY=1` and serves the IPC socket without
  initializing the system tray.

---

### G6 — Cross-Platform

**Goal**: The daemon runs on macOS, Linux (x86_64 + aarch64), and Windows.

**Acceptance criteria**:
- CI builds for all three platforms pass.
- Socket path falls back correctly per platform (Unix socket → named pipe).
- Sidecar binary is bundled for each target via Tauri's `externalBin`.

---

## Flutter App Scope (Out of Scope for This Repo)

The Flutter application is a **separate project**. This Rust/Tauri repo only defines and
implements the daemon-side contract. The Flutter side is responsible for:

- Connecting to the IPC socket and implementing the JSON-RPC client
- All UI: connection screen, settings, core selector, logs viewer, tray integration
  (Flutter system tray via `tray_manager` package)
- Platform channels for anything Flutter cannot do via the IPC socket
- iOS / Android companion (future: connects to desktop daemon over LAN, or runs its own
  embedded Rust daemon via FFI)

The daemon intentionally has no knowledge of Flutter's routing, state management library,
or UI design system.

---

## Development Phases

| Phase | Deliverable | Status |
|-------|-------------|--------|
| 1 | `domain` — pure contracts | ✓ done |
| 2 | `data` — storage impls | ✓ done |
| 3 | `config` — multi-source config | ✓ done |
| 4 | `core-singbox-standalone` — process wrapper + reaper | ✓ done |
| 5 | `service` — VpnManager orchestrator | ✓ done |
| 6 | `cli` — headless binary | ✓ done |
| 7 | `app` — system tray + Tauri IPC commands | ✓ done |
| 8 | **IPC server** — JSON-RPC socket in `app` | next |
| 9 | **Flutter client** — Dart JSON-RPC client + UI | separate repo |
| 10 | **Mobile companion** — iOS/Android Flutter + embedded daemon | future |

---

## Dependency Rule

Inner crates never depend on outer crates. The dependency graph is a strict DAG:

```
domain          → (nothing)
data            → domain
config          → serde only
core-singbox-standalone → domain, util
core-mock       → domain
service         → domain  (data in dev-dependencies only)
cli             → config, core-singbox-standalone, service
app             → service, core-singbox-standalone, config, data
                  + tauri, tauri-plugin-shell, tauri-plugin-store,
                    tauri-plugin-dialog
```

`domain` stays free of Tauri, serde, and async runtimes forever. This ensures the core
business logic is testable without Tauri and portable to the Flutter FFI layer if needed.
