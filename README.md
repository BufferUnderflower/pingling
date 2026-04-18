# Pingle — Rust/Tauri VPN Daemon

Pingle is the native backend daemon for a cross-platform VPN client. It owns all VPN
lifecycle logic (process management, config, settings, tray) and exposes a typed
**JSON-RPC 2.0 interface over a local Unix socket** (named pipe on Windows) that the
Flutter UI connects to.

**The Flutter UI lives in a separate repository.** This repo is the daemon only.
Client-side IPC consumption now lives in the separate sibling repo
`../pingle-ipc`, which is a Rust-only consumer library. This daemon does not
depend on that repo; it only exposes the JSON-RPC contract that clients
consume.

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design, IPC protocol
specification, domain breakdown, and acceptance criteria per feature.

---

## What this daemon does

- Manages the sing-box (or xray) VPN process as a sidecar binary
- Exposes VPN lifecycle commands over a local IPC socket (JSON-RPC 2.0)
- Pushes real-time state change events to connected Flutter clients
- Runs a system tray (red/yellow/green icon) for quick access without the Flutter UI
- Detects external process death and notifies clients within 500 ms

## What it does NOT do

- No webview, no HTML/CSS/JS frontend
- No Tauri `invoke()` bridge (that requires a webview — we don't use one)
- No UI logic — the Flutter app owns all display and interaction

---

## Architecture (short form)

```
Flutter App (separate repo)
    │  JSON-RPC 2.0
    │  Unix socket / named pipe
    ▼
Tauri Daemon (this repo)           ← headless, system tray only
    │
    ├── app/          IPC server · tray · sidecar resolution
    ├── service/      VpnManager + typed Pipeline<Op> fields
    │   └── middleware/   LoggingMiddleware, GeoFilterMiddleware, ...
    │   └── handlers.rs   ConnectHandler, DisconnectHandler, ...
    ├── domain/       VpnCore trait + Pipeline/Middleware/Handler (zero deps)
    │   └── ops/      OpConnect, OpListOutbounds, ... (typed operations)
    ├── data/         SettingsStorage impls (memory, tauri-store)
    ├── config/       Multi-source config (YAML/JSON/env)
    ├── core-singbox-standalone/   sing-box process wrapper + reaper
    ├── core-mock/    MockVpnCore for tests
    └── cli/          Headless CLI binary (scripting / CI)
    │
    ▼
sing-box binary (bundled sidecar)  ← owns the actual VPN tunnel
```

Every VPN operation flows through a typed middleware pipeline (inspired by
tower/Envoy): `input -> Middleware A -> Middleware B -> Handler -> output`.
Lifecycle pipelines (connect, disconnect, ...) always exist. Capability
pipelines (list outbounds, select outbound, test latency) are `Option` --
their presence IS the capability declaration.

Full architecture: [ARCHITECTURE.md](./ARCHITECTURE.md)

---

## Build

```bash
nix-shell              # enter dev environment (Rust + pkg-config)
cargo test             # run all tests
cargo check            # type-check
cargo build            # build all crates
cargo run -p cli -- status   # headless CLI
cargo tauri dev              # run Tauri daemon with tray (no window)
```

## Project layout

```
pingle/
├── ARCHITECTURE.md             # Full system design + IPC protocol
├── domain/                     # Pure traits + types (zero deps)
├── data/                       # SettingsStorage impls
├── config/                     # Multi-source config loader
├── core-singbox-standalone/    # sing-box process wrapper
├── core-mock/                  # MockVpnCore for tests
├── service/                    # VpnManager orchestrator
├── cli/                        # Headless CLI binary
├── app/                        # Tauri daemon (IPC server + tray)
│   ├── src/main.rs
│   ├── tauri.conf.json
│   ├── capabilities/
│   └── icons/                  # tray-connected/connecting/disconnected.png
├── docs/
│   └── comparison.md           # IPC transport options comparison
└── shell.nix                   # Nix dev environment
```

## Dependency rule

`domain` → nothing. All other crates depend inward. Tauri never appears in `domain`,
`data`, `config`, `core-*`, or `service`. This ensures business logic is testable
without Tauri and is portable to the Flutter FFI layer if needed.
