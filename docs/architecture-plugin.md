# Plugin architecture

**Status:** implemented (2026-04-08), extended with the slot-chain
middleware framework (2026-04-12 — see
[`plugin-slots.md`](plugin-slots.md) for the current wire shape and
canonical slot catalog). The fundamentals below still stand: one
`Plugin` trait, one wasm runtime, plugins own their vocabulary.
What changed: the daemon now also calls plugins in a
`before → exec → after` chain around named extension points, on
top of the same `handle_ipc` dispatcher documented here. New
readers should skim this file for the *rationale*, then jump to
`plugin-slots.md` for the wire protocol details.

## Problem

The Pingle VPN daemon is intentionally **open-source at the core** —
VPN tunnel management, core registry, IPC server. But it has to
work alongside **closed-source vendor concerns**: subscription
backends, panel APIs, billing, server allocation. Those concerns
must:

1. **Plug into the daemon at runtime** — no compile-time link to any
   vendor crate. The OSS daemon must build, run, and pass all tests
   without any vendor source on disk.
2. **Define their own vocabulary.** Method names like `auth.login`,
   `profile.bootstrap`, `account.config` belong to the *vendor's
   product surface*, not the daemon's. The daemon must not enumerate,
   document, or validate them.
3. **Live in any language.** A vendor wanting to ship a plugin in
   Go, Zig, AssemblyScript, or another Rust workspace should not
   have to fork the daemon repo.
4. **Be sandboxed.** A misbehaving (or malicious) plugin cannot
   crash the daemon, exfiltrate to arbitrary hosts, or spin
   indefinitely.

## Solution: a tiny Plugin trait + a wasm runtime

Three layers, cleanly separated:

```
  ┌─────────────────────────────────────────────────────────────┐
  │                      GUI / TUI / CLI                        │
  │              (Flutter, nocterm, headless cli)               │
  │  built-in: vpn.connect, core.list, daemon.info, ...         │
  │  plugin-defined: auth.login, profile.bootstrap, ...         │
  │  (clients learn plugin-defined names from PLUGIN docs,      │
  │   not from the daemon)                                       │
  └───────────────────────────┬─────────────────────────────────┘
                              │ JSON-RPC 2.0 over UDS + TCP
                              ▼
  ┌─────────────────────────────────────────────────────────────┐
  │                     ipc-server::methods                      │
  │  built-in arms (vpn.* / core.* / config.* / outbounds.* /   │
  │  daemon.*) dispatch directly. Anything else falls through   │
  │  to vpn.plugin().handle_ipc(method, params), which returns  │
  │  Some(ok/err) (claimed) or None (not claimed → MethodNotFound).
  └───────────────────────────┬─────────────────────────────────┘
                              │
                              ▼
  ┌─────────────────────────────────────────────────────────────┐
  │                     service::VpnManager                     │
  │  holds `Option<Arc<dyn Plugin>>` via `set_plugin`           │
  └───────────────────────────┬─────────────────────────────────┘
                              │
                              ▼
  ┌─────────────────────────────────────────────────────────────┐
  │                  domain::traits::plugin                     │
  │  Plugin: name + authenticator() + handle_ipc(method, params)│
  │  Authenticator: is_authenticated + user_id                  │
  │  No `login`, no `bootstrap`, no value types. Tiny + generic.│
  └───────────────────────────┬─────────────────────────────────┘
                              │
                              ▼
  ┌─────────────────────────────────────────────────────────────┐
  │     plugin_extism::plugin_adapter::PluginAdapter            │
  │  bridges the trait to a wasm plugin: every method becomes   │
  │  one extism call_json against `plugin_handle_ipc` (or       │
  │  `plugin_authenticator_status`), with a JSON wire envelope. │
  └───────────────────────────┬─────────────────────────────────┘
                              │  extism guest call (JSON in / out)
                              ▼
  ┌─────────────────────────────────────────────────────────────┐
  │   ANY .wasm PLUGIN                                          │
  │  exports `plugin_handle_ipc` (required) and                  │
  │  `plugin_authenticator_status` (optional).                   │
  │  Internally: a router over the plugin's own method names,   │
  │  using `extism_pdk::http::request` to reach the panel,      │
  │  bound to the host's allowed_hosts whitelist.               │
  └─────────────────────────────────────────────────────────────┘
```

## The trait surface

```rust
pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;

    fn authenticator(&self) -> Option<&dyn Authenticator>;

    fn handle_ipc(&self, method: &str, params: &serde_json::Value)
        -> Option<Result<serde_json::Value, VpnError>>;
}

pub trait Authenticator: Send + Sync {
    fn is_authenticated(&self) -> bool;
    fn user_id(&self) -> Option<String>;
}
```

That's the **whole** trait. No vendor-specific value types, no
named auth modes, no `login`/`bootstrap`/`checkout` methods. The
daemon does not know what the plugin can do; it only knows that the
plugin can claim or pass on JSON-RPC method calls and may surface
an authenticator probe.

### Why so small

Earlier drafts had a typed `UserApi` trait with named methods
(`login`, `bootstrap`, `list_outbounds`, `baked_config`, `checkout`)
and value types (`AuthMode`, `Session`, `UserBootstrap`, `Wallet`,
`Order`, `Checkout`). Every one of those names is **vendor product
surface** that the public daemon should not name. Adding a new
endpoint to a plugin shouldn't require any change to the daemon's
trait, ipc-server arms, or wire-protocol constants — yet under the
old design all three needed updates per endpoint.

The new trait has **two methods** that together can express any
plugin shape, with the daemon staying ignorant of the plugin's
vocabulary. New endpoint = one row in the plugin's internal router.
Zero daemon changes.

### Why the authenticator stub-trait at all

It's the one piece of cross-cutting state clients want to render in
their chrome ("Logged in as Alice" / "Login" button) without
dispatching a JSON-RPC call per frame. The probe is `is_authenticated()`
+ optional `user_id()` — both cheap snapshot reads. Everything else
about the auth flow (login, logout, refresh, token storage) lives
inside the plugin and is dispatched via `handle_ipc` under whatever
method names the plugin chooses.

## The wasm wire contract

`plugin-extism::plugin_adapter::PluginAdapter` is the canonical bridge
from `Plugin` to a wasm guest. Two exports:

| Wasm export                    | When called                                | Input                                              | Output                                                                                            |
|--------------------------------|--------------------------------------------|----------------------------------------------------|---------------------------------------------------------------------------------------------------|
| `plugin_handle_ipc` (required) | `Plugin::handle_ipc`                       | `{"method": "string", "params": <any json>}`       | `{"handled": true, "result": <json>}` / `{"handled": true, "error": "msg"}` / `{"handled": false}` |
| `plugin_authenticator_status` (optional) | `Plugin::authenticator()` (refresh) | `null`                                             | `{"is_authenticated": bool, "user_id": "..."}`                                                    |

- A wasm file that does not export `plugin_handle_ipc` is rejected
  by `looks_like_plugin` and not installed. Discovery logs a clean
  warning and continues to the next file.
- A wasm file that exports `plugin_handle_ipc` but not
  `plugin_authenticator_status` is loaded fine; the adapter's
  `Plugin::authenticator()` returns `None` and the daemon renders
  an "anonymous" state.
- Adding a new IPC endpoint to a plugin requires adding a new arm
  in the plugin author's own `plugin_handle_ipc` router. The
  daemon, the host trait, the wire constants, and the IPC
  dispatcher require **zero changes**.

## Plugin distribution

The wasm plugin is a self-contained `.wasm` file. The daemon
discovers it at startup by scanning the platform plugins dir:

- Linux/BSD: `$XDG_CONFIG_HOME/pingle/plugins/` (default `~/.config/pingle/plugins/`)
- macOS: `~/Library/Application Support/pingle/plugins/`
- Windows: TBD

Override via `PinglingConfig::plugins.plugins_dir` (config file or
`PINGLING_PLUGINS_DIR` env var).

The first `.wasm` file in the directory whose exports satisfy
`looks_like_plugin` wins. The daemon loads it via
`PluginAdapter::load(path, allowed_hosts)`, where `allowed_hosts`
is the daemon's per-build whitelist of HTTPS hosts the plugin is
allowed to reach (e.g. `["example.com"]` for the Pingle
build). Anything not in the list is rejected by extism's manifest
sandbox before the request leaves the guest.

Per-call wall-clock budget is set to 30s by the daemon. Plugins
that hang past that get a clean error returned to the caller.

## Adding a new plugin (e.g. 3x-ui)

1. **Create a wasm crate.** It can live anywhere — its own repo, a
   sub-crate of an existing tools repo, whatever. The crate needs
   `extism-pdk = "1"`, `[lib] crate-type = ["cdylib"]`, and a
   `[workspace]` block (so it doesn't try to merge into a parent
   workspace). See
   [`plugin-extism/tests/fixtures/plugin_mock/Cargo.toml`](../plugin-extism/tests/fixtures/plugin_mock/Cargo.toml)
   for the canonical minimal setup.
2. **Export `plugin_handle_ipc`.** It takes a JSON string of shape
   `{"method": "...", "params": <any>}` and returns a JSON string
   of shape `{"handled": true, "result": <any>}` /
   `{"handled": true, "error": "msg"}` / `{"handled": false}`.
   Inside, route on `method` and call your panel's HTTP API with
   `extism_pdk::http::request`.
3. **(Optional) Export `plugin_authenticator_status`.** Returns
   `{"is_authenticated": bool, "user_id": "..."}` so the daemon
   can drive UI auth hints. Skip this entirely if your plugin is
   identity-agnostic (e.g. observability-only).
4. **Document your method names** in the plugin's own README. The
   daemon will not enumerate them; clients (TUI, Flutter) hardcode
   the calls they want to make. Pick stable names — they're a
   contract with your users, not with the daemon.
5. **Build to wasm.** `cargo build --release --target wasm32-unknown-unknown`
6. **Drop the `.wasm` into the platform plugins dir.** The next
   daemon startup picks it up. No daemon rebuild required.

A plugin author does **not** need to modify `domain`, `service`,
`ipc-server`, `app`, or any wire-protocol constants. All changes
stay inside the .wasm.

## How clients learn the plugin's surface

Clients (TUI, Flutter, CLI) learn the plugin's method names from
the plugin's documentation, not from the daemon. The TUI's profile
screen knows to call `auth.login` because the *Pingle hub plugin's*
README says so — the daemon's wire-protocol-constants file does
not mention `auth.login` at all.

`daemon.info` returns a small `plugin` field clients can render in
their chrome:

```json
{
  "plugin": {
    "name": "pingle-hub-userapi",
    "authenticator": {
      "is_authenticated": true,
      "user_id": "alice@pingle"
    }
  }
}
```

`plugin` is `null` when no plugin is installed; `authenticator` is
absent when the plugin doesn't expose one. Clients use this to
show "Plugin: pingle-hub-userapi · alice" in their status bar.

## Sequence: TUI login → profile → connect

```
 TUI         ipc-server        VpnManager     PluginAdapter        wasm guest         panel REST
   │             │                  │                  │                  │                  │
   │ daemon.info │                  │                  │                  │                  │
   │────────────>│                  │                  │                  │                  │
   │             │ vpn.plugin()     │                  │                  │                  │
   │             │─────────────────>│                  │                  │                  │
   │             │ <─ Some(plugin)  │                  │                  │                  │
   │             │ plugin.authenticator() (refresh)     │                  │                  │
   │             │─────────────────────────────────────>│                  │                  │
   │             │                  │                  │ call_json(       │                  │
   │             │                  │                  │ plugin_authenticator_status, null)   │
   │             │                  │                  │─────────────────>│                  │
   │             │                  │                  │ <─ {is_authenticated: false}        │
   │<─ {plugin: {name, authenticator: {is_authenticated: false}}}                             │
   │             │                  │                  │                  │                  │
   │ auth.login (plugin-defined)    │                  │                  │                  │
   │────────────>│                  │                  │                  │                  │
   │             │ (built-in arms don't claim auth.login)                  │                  │
   │             │ vpn.plugin().handle_ipc("auth.login", params)           │                  │
   │             │─────────────────────────────────────>│                  │                  │
   │             │                  │                  │ call_json(       │                  │
   │             │                  │                  │ plugin_handle_ipc, {method, params}) │
   │             │                  │                  │─────────────────>│                  │
   │             │                  │                  │                  │ http::request(   │
   │             │                  │                  │                  │  POST /auth/...) │
   │             │                  │                  │                  │─────────────────>│
   │             │                  │                  │                  │ <─ Session       │
   │             │                  │                  │ <─ {handled: true, result: {...}}   │
   │             │ <─ Some(Ok(Session JSON))            │                  │                  │
   │<─ {token, account_id, ...}     │                  │                  │                  │
   │             │                  │                  │                  │                  │
   │ profile.bootstrap (plugin-defined)                  │                  │                  │
   │────────────>│ ... same fall-through path ...        │                  │                  │
   │             │                  │                  │                  │                  │
   │ vpn.connect (built-in)         │                  │                  │                  │
   │────────────>│                  │                  │                  │                  │
   │             │ ConnectHandler runs the existing pipeline               │                  │
   │             │ <─ connected     │                  │                  │                  │
   │<─ ok        │                  │                  │                  │                  │
```

The daemon's IPC layer is the only place where built-in vs
plugin-defined methods differ — and the difference is one fall-
through clause in `methods.rs`. Everything else is uniform.

## What the OSS daemon looks like with no plugin installed

- `daemon.info` returns `"plugin": null`
- Plugin-defined methods return `MethodNotFound` cleanly
- Everything else (vpn.connect, core.list, config.set, ...) works
  normally
- TUI screens that need plugin features (the profile/login screen)
  show "no plugin installed" placeholder text and disable their
  buttons

There are zero `auth not configured` strings, zero special-case
error codes, zero plugin-vocabulary leaks in the daemon's source.

## Testing strategy

| Layer | Test type | Count |
|---|---|---|
| `domain::traits::plugin` | unit, NullPlugin + StubPlugin | 4 |
| `service::VpnManager` | unit, set_plugin / plugin / replace | 3 |
| `ipc-server::methods` plugin fall-through | dispatch tests with stub plugin (claim/error/unclaim, daemon.info plugin meta) | 6 |
| `plugin-extism::plugin_adapter` | unit, JSON wire envelope round-trip | 6 |
| `plugin-extism` integration smoke | builds the wasm fixture in `tests/fixtures/plugin_mock`, loads it via `PluginAdapter::load`, round-trips `handle_ipc` for claim/error/unclaim/auth | 1 |
| `app::discover_plugin` | unit, missing dir / empty dir / garbage wasm / filename filter | 6 |

The integration smoke test catches drift between the trait shape
and the wasm wire format on either side. SKIPs gracefully when
`wasm32-unknown-unknown` is missing.

## Future work

- **Per-plugin allowed-hosts override.** Today the daemon hardcodes
  `DEFAULT_PLUGIN_ALLOWED_HOSTS = ["example.com"]`. For a
  multi-plugin world, lift this into a per-plugin sidecar (a
  `<plugin>.toml` next to the `.wasm`).
- **Multiple plugins at once.** Today only one plugin can be
  installed. The slot grows to `Vec<(name, Arc<dyn Plugin>)>` and
  the IPC fall-through walks the list in order until one returns
  `Some(...)`. The first claim wins.
- **Host functions for secret storage.** The plugin currently caches
  its token in wasm guest memory (lost across daemon restarts).
  Adding host functions like `host_secret_get(key) -> Option<String>`
  / `host_secret_set(key, value)` lets the plugin persist tokens
  through Tauri's native secret-storage plugins (`tauri-plugin-store`,
  `tauri-plugin-stronghold`, OS keychains).
- **Plugin signature verification.** A vendor-signed `.wasm` plus a
  `.sig` file alongside; daemon refuses to load a plugin whose
  signature doesn't match a pinned public key.
- **Async trait.** Today the trait is sync. If wasm plugins ever
  need to interleave multiple in-flight requests, an async trait
  would let extism's async API surface concurrency. Current shape
  is one HTTP request per call so this isn't motivating yet.
- **Richer error envelopes.** Today plugin errors collapse to
  `VpnError::Unknown(message)`. Extending the wire envelope from
  `{"error": "msg"}` to `{"error": {"code": "...", "message": "..."}}`
  lets clients distinguish auth-expired / network-unreachable /
  payment-required without regex-matching the human message.

## See also

- `domain/src/traits/plugin.rs` — the trait + tests
- `plugin-extism/src/plugin_adapter.rs` — wasm bridge + JSON wire envelope
- `plugin-extism/tests/fixtures/plugin_mock/src/lib.rs` — minimal worked example
- `plugin-extism/tests/plugin_adapter_smoke.rs` — end-to-end test
- `ipc-server/src/methods.rs` — the plugin fall-through dispatcher
- `app/src/main.rs` — `discover_plugin`, the runtime plugin scan
