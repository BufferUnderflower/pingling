# Windows action plan

Counterpart to `core-libbox-macos/README.md`'s rollout document. Tracks
the work to bring the pingle daemon up on Windows with the same
in-process sing-box engine the macOS build uses.

## Mental model

```
┌────────────────────────────────────────────────────────────────────┐
│                       pingle daemon (.exe, user mode)               │
│                                                                    │
│   ┌──────────────────┐   ┌────────────────────────────────────┐    │
│   │  Tauri tray app  │   │  ipc-server (UDS / TCP / discovery)│    │
│   └──────────────────┘   └────────────────────────────────────┘    │
│                ▲                          ▲                        │
│                │                          │                        │
│                │       service::VpnManager (one slot per role)     │
│                │                          │                        │
│   ┌────────────┴──────────────────────────┴──────────────────┐     │
│   │   core-libbox-windows  ◄──── this crate, this skeleton   │     │
│   │   (libbox.dll loaded in-process via the C bridge)        │     │
│   └────────────┬──────────────────────────────────────────────┘    │
│                │                                                    │
└────────────────┼────────────────────────────────────────────────────┘
                 │  RPC over named pipe (TBD — same shape as
                 │  the macOS XPC channel between host and SystemExt)
                 ▼
   ┌──────────────────────────────────────────────────────────────┐
   │  service-host-windows (.exe, runs as a Windows Service)      │
   │  - Owns the TUN device (WinTun driver)                       │
   │  - Drops privileges except for `SeChangeNotifyPrivilege`     │
   │    + the bare minimum needed by sing-box's tun inbound       │
   │  - Re-exec'd by SCM on crash (lifetime owner of the tunnel)  │
   └──────────────────────────────────────────────────────────────┘
```

## Stages

### Stage 1 — `core-libbox-windows` skeleton (DONE this round)

Crate exists with:
- `Cargo.toml` mirroring `core-libbox-macos`
- `build.rs` that auto-detects `frameworks/libbox/{libbox.dll,lib,h}`
  or `PINGLE_LIBBOX_WINDOWS_DIR`, and falls back to a `cfg(libbox_stub)`
  build on every host that doesn't have those artifacts
- `bridge/libbox_bridge.c` C shim with `pingle_libbox_*` wrapper
  signatures matching the macOS bridge surface (currently returning
  stub sentinels)
- `src/bridge.rs` Rust FFI declarations
- `src/core.rs` `VpnCore` impl with full lifecycle in real mode and
  uniform `PrerequisiteMissing` errors in stub mode
- 4 stub-mode unit tests covering construct / start / check_prerequisites /
  info, exercised by `cargo test --workspace` on every host

The whole workspace (`cargo build --workspace`) stays green on macOS
without a Windows toolchain. CI is unchanged.

### Stage 2 — gobind libbox build

Pull sing-box's `cmd/libbox` Go target and build it with `gobind` for
the `windows/amd64` target. Output is `libbox.dll` + `libbox.lib` +
`libbox.h`. Initially do this manually on a Windows VM, then promote
to a CI job.

Key build flags:
- `CGO_ENABLED=1`
- MSVC toolchain (gobind also supports MinGW but the Tauri side is
  MSVC, so matching avoids ABI surprises)
- Strip debug symbols for release; retain for dev
- `-buildmode=c-shared`

Drop the artifacts under `core-libbox-windows/frameworks/libbox/` (or
set `PINGLE_LIBBOX_WINDOWS_DIR`) and re-run `cargo build -p
core-libbox-windows --target x86_64-pc-windows-msvc`. The shim's stub
sentinels then need replacing with real `LibboxNewService` /
`LibboxBoxService_*` calls — that's a one-line change per function
once the gobind symbol names are known.

### Stage 3 — TUN inbound (WinTun)

`sing-box`'s `tun` inbound on Windows uses
[WinTun](https://www.wintun.net/). `wintun.dll` ships next to the
daemon `.exe` and is loaded by sing-box at runtime when a `tun`
inbound is configured. No work needed in this crate — sing-box does
the loading itself — but the bundler step (Stage 6) has to copy
`wintun.dll` into the install dir alongside `libbox.dll`.

WinTun requires the daemon to run with elevated privileges to create
the adapter. That's why Stage 4 splits the privileged side off into
a separate process.

### Stage 4 — `service-host-windows` (privileged side)

New crate. Runs as a Windows Service registered via SCM
(`sc.exe create pingle-vpn ...` or programmatically via `windows-service`
crate). Responsibilities:

- Own the WinTun adapter and the libbox `BoxService` instance (the
  thing that needs `SeCreateGlobalPrivilege`)
- Expose a small RPC over a named pipe at `\\.\pipe\pingle\service` —
  same JSON-RPC dialect the daemon already speaks for IPC, just over
  a different transport
- Receive `start(config_path)` / `stop()` / `status()` from the
  unprivileged daemon and proxy them to libbox
- Re-exec on crash (SCM does this automatically)

The unprivileged daemon's `core-libbox-windows` becomes a thin RPC
client to this service in production builds. Dev builds keep the
direct in-process libbox call for fast iteration.

### Stage 5 — netwatcher (DONE this round — `pingle-netwatch` crate)

Cross-platform native crate that wraps the `netwatcher` crate from
crates.io (which itself abstracts `NotifyIpInterfaceChange` on
Windows, `SystemConfiguration` on macOS, `netlink` on Linux). Exposes
a `Watcher` trait + `UpdateEvent` channel the daemon links against
directly — no IPC, no wasm — and an optional in-process
`NetwatchPlugin` hook for debugging / policy injection (passthrough
default).

The earlier draft of this section described a wasm-plugin shape with
`netwatcher.subscribe` / `netwatcher.snapshot` method names. That was
wrong: network change notifications are a *platform* abstraction, not
a vendor concern, and wasm guests can't access platform syscalls
without host-imported functions anyway. Native crate + optional
in-process hook is the right shape.

See `pingle-netwatch/README.md`.

### Stage 6 — config processor pipeline + strategy retry (DONE this round)

Three new crates + one new `WrapHook<OpConnect>`:

- **`core-config-processor`** — direct port of the dart `singbox_config`
  package's processor pipeline to native Rust. Owns the bulletproof
  native ruleset downloader (sing-box's own ruleset fetcher is flaky on
  20–50% of users on Windows; manual download + on-disk cache is
  bulletproof and faster). Seven processors: dns, ruleset, routing_excl,
  stack, log, clash_api, platform.

- **`pingle-pipeline-plugin`** — optional extism plugin slot with a
  stage-aware contract. Plugins claim stages via
  `pipeline_capabilities` and the daemon only invokes them at claimed
  stages. Seven stages from day one. Passthrough default — no wasm
  on disk = native behavior unchanged.

- **`StrategyRetryWrap`** in `service::middleware::strategy_retry` —
  `WrapHook<OpConnect>` that owns the strategy iteration + retry loop.
  Resolves a `StrategyPlan` from per-call metadata override OR the
  active core's `default_strategy_plan()` trait method. For each
  strategy: runs the native pipeline, calls the plugin slot per
  claimed stage, hands off to the inner handler. On failure: classifies
  via the small stable `ErrorKind` taxonomy and decides retry / advance
  / bail per the documented action table.

- **Per-core defaults**: `core-libbox-macos`, `core-libbox-windows`,
  and `core-singbox-standalone` each implement `default_strategy_plan()`
  with a tuned plan. The Windows plan is longest (4 strategies, 120s
  global) because that's where the historical pain lives — half users
  smooth on one combo, half on another.

See:
- `core-config-processor/README.md` for the error→action table
- `pingle-pipeline-plugin/src/protocol.rs` for the wire-format types
- `service/src/middleware/strategy_retry.rs` for the loop algorithm
- `docs/superpowers/specs/2026-04-08-pingle-netwatch-config-pipeline-design.md`
  for the full design

### Stage 7 — bundler

WiX or NSIS installer that:
- Stages `pingle.exe`, `libbox.dll`, `wintun.dll`, the wasm plugins,
  and the SCM service registration
- Signs everything with the EV cert (Windows Defender SmartScreen
  requires EV for warning-free first launch)
- Installs the daemon as a per-user app + the service-host as a
  per-machine LocalSystem service

## Why split host + service like this

The macOS build splits host (Tauri tray) from privileged extension
(SystemExtension) for the same reasons:
- The privileged side has a strict capability scope (TUN + a few
  syscalls) and survives crashes
- The unprivileged side renders UI, talks to the user, manages the
  plugins, and can be killed/restarted without dropping the tunnel
- Code-signing trust boundaries are clearer when the privileged
  binary is its own thing

Windows has the same shape — it just uses Service + named pipe
instead of SystemExtension + XPC.

## What this skeleton is NOT

- It is **not** a sing-box CLI subprocess wrapper — that's
  `core-singbox-standalone`, which already runs on Windows and is the
  fallback while the libbox path is being built.
- It is **not** the privileged service. That's a separate crate.
- It is **not** complete. The shim returns stubs until a real
  `libbox.dll` is dropped in.

## Open questions

- gobind output for libbox: confirm it produces a flat C API or
  whether we need to wrap a class-based one (gobind has both modes)
- WinTun licence: GPL with an exception for `wintun.dll` linking;
  confirm with legal before bundling
- WinUI 3 vs WebView2 vs the Tauri Windows tray for the host — Tauri
  is the simplest path and matches the macOS code, sticking with it
- Auto-update: Tauri's updater works on Windows, but the privileged
  service-host needs MSI/Restart-Manager-aware updates
