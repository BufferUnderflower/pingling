# Contributing to Pingle

Pingle is the Rust/Tauri backend daemon for a cross-platform VPN client. The Flutter UI
is a **separate repository** — this repo is the daemon only.

See [ARCHITECTURE.md](./ARCHITECTURE.md) for the full system design. The short version:

```
Flutter App  →  JSON-RPC socket  →  Tauri Daemon (this repo)  →  sing-box binary
```

---

## Stack

- **Rust** — business logic, VPN process management, IPC server
- **Tauri v2** — system tray, sidecar bundling, macOS/Linux/Windows packaging
- **Nix** (optional) — reproducible dev environment (`shell.nix`)

## Quick start

```bash
nix-shell                          # enter dev environment (or use rustup)
cargo check                        # type-check all crates
cargo test                         # run all tests
cargo build                        # full build
cargo run -p cli -- status         # headless CLI smoke test
```

## Workspace crates

| Crate | Role |
|-------|------|
| `domain` | Pure traits + types. Zero deps. Never touches Tauri. |
| `data` | `SettingsStorage` impls: `MemorySettingsStorage` (tests), `TauriStoreSettings` (daemon) |
| `config` | `ConfigLoader` — merges YAML/JSON/env vars into `PinglingConfig` |
| `core-singbox-standalone` | `VpnCore` impl via `std::process::Command`. No Tauri dep. |
| `core-mock` | `MockVpnCore` for unit tests |
| `service` | `VpnManager` — orchestrates core + storage + typed middleware pipelines |
| `cli` | Headless `clap` binary: `start/stop/status/restart/validate/info/prereqs` |
| `app` | Tauri daemon: IPC server (JSON-RPC socket) + system tray + sidecar |

## Dependency rule

Inner crates must never import outer crates. `domain` has zero non-std deps.
`core-singbox-standalone` and `service` must not depend on Tauri.

```
domain → (nothing)
data → domain
config → serde only
core-singbox-standalone → domain, util
service → domain
cli → config, core-singbox-standalone, service
app → service, core-singbox-standalone, config, data, tauri, tauri-plugin-*
```

## IPC: how Flutter talks to the daemon

The Tauri daemon exposes a **JSON-RPC 2.0 server** on a Unix domain socket
(`$TMPDIR/pingle.sock` / `\\.\pipe\pingle` on Windows). Flutter connects with a Dart
socket client. Tauri's built-in `invoke()` bridge is **not used** — it requires a
webview which we intentionally omit.

Full protocol spec: [ARCHITECTURE.md — IPC Protocol](./ARCHITECTURE.md#ipc-protocol-tauri-daemon--flutter)

## TDD workflow

1. Write or update a trait in `domain` (contract first)
2. Write a failing test in the consuming crate using `MockVpnCore` or
   `MemorySettingsStorage` (RED)
3. Implement the minimum code to pass (GREEN)
4. Refactor

All business logic tests must run without Tauri, without a sing-box binary, and without
a display. If a test requires a real binary, use `/bin/sleep` or `/bin/echo` as a stand-in.

## Key traits

```rust
// domain/src/traits/vpn_core.rs — lifecycle contract for VPN engines
pub trait VpnCore: Send + Sync {
    fn start(&mut self, config_path: &str) -> Result<(), VpnError>;
    fn stop(&mut self) -> Result<(), VpnError>;
    fn kill(&mut self) -> Result<(), VpnError>;
    fn restart(&mut self, config_path: &str) -> Result<(), VpnError>;
    fn status(&self) -> ConnectionState;
    fn running(&self) -> bool;
    fn info(&self) -> CoreInfo;
    fn validate_config(&self, config_path: &str) -> Result<(), VpnError>;
    fn check_prerequisites(&self) -> Vec<PrerequisiteCheck>;
    fn subscribe(&self) -> Option<mpsc::Receiver<CoreEvent>>;
}

// domain/src/pipeline.rs — typed middleware pipeline (tower/Envoy-inspired)
pub trait Operation: Send + Sync + 'static {
    type Input: Send;
    type Output: Send;
    fn name() -> &'static str;
}

pub trait Handler<Op: Operation>: Send + Sync {
    fn handle(&self, input: Op::Input) -> Result<Op::Output, VpnError>;
}

pub trait Middleware<Op: Operation>: Send + Sync {
    fn name(&self) -> &str;
    fn priority(&self) -> u32 { 100 }       // lower = outermost
    fn handle(&self, input: Op::Input, next: &dyn Handler<Op>) -> Result<Op::Output, VpnError>;
}
```

Pipeline composition: `Pipeline::new(handler).push(middleware)`. Middleware runs in
priority order (lower first). Each middleware can modify input, modify output,
short-circuit, or observe.

## Adding a new VPN core

1. Create `core-<name>-standalone/` crate, depend only on `domain`
2. Implement `VpnCore` for `<Name>Standalone`
3. Include a reaper thread using `child.try_wait()` to detect unexpected exits
4. Register in `app/src/main.rs` -> `build_registry()` under a feature flag
5. If the core supports optional capabilities (outbound listing, selection, latency
   testing), implement `Handler<OpListOutbounds>` etc. and register the pipeline via
   `vpn_manager.set_list_outbounds(Pipeline::new(Box::new(MyHandler)))`
6. Zero changes needed in `service`, `data`, or `domain`

The new core appears automatically in the Flutter core-selector via `core.list` IPC call.
The `capabilities()` method reports which optional pipelines are registered -- the
presence of a pipeline IS the capability declaration.

## Adding an IPC method

1. Add the handler in `app/src/main.rs` → JSON-RPC dispatch table
2. Add it to the Dart client in the Flutter repo
3. Document the request/response shape in [ARCHITECTURE.md](./ARCHITECTURE.md)

## Mutex safety rule

All `Mutex::lock()` calls must use `unwrap_or_else(|e| e.into_inner())` -- never `.unwrap()`.
Panics from lock poisoning must not propagate to IPC clients or crash the daemon.
Terminal handlers (ConnectHandler, etc.) follow this rule for `Arc<Mutex<CoreRegistry>>`.

## Code style

- Doc comments on all public items
- `snake_case` functions, `PascalCase` types, `SCREAMING_SNAKE_CASE` consts
- One trait per file in `traits/`
- Tests in `#[cfg(test)] mod tests` at the bottom of the same file
- `MemorySettingsStorage` for all unit tests (never `TauriStoreSettings`)
- No `#[allow(unused)]` — remove dead code instead

## Nix notes

Rustup's `rustc` requires GLIBC 2.39+ (`pidfd_spawnp`). Nix-shell default is 2.38.
Fix in `shell.nix`:

```nix
export RUSTFLAGS="-C link-arg=-Wl,--dynamic-linker=<glibc-2.40-ld-path> $RUSTFLAGS"
```

Full details: `.opencode/skills/tauri-nix-setup/SKILL.md`
